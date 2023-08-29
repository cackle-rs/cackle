use crate::checker::BinLocation;
use crate::location::SourceLocation;
use crate::names::DebugName;
use crate::names::Namespace;
use crate::names::SymbolAndName;
use crate::symbol::Symbol;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use fxhash::FxHashMap;
use gimli::Attribute;
use gimli::AttributeValue;
use gimli::Dwarf;
use gimli::EndianSlice;
use gimli::LittleEndian;
use gimli::Unit;
use gimli::UnitOffset;
use std::ffi::OsStr;
use std::os::unix::prelude::OsStrExt;
use std::path::Path;

#[derive(Default)]
pub(crate) struct DebugArtifacts<'input> {
    pub(crate) symbol_debug_info: FxHashMap<Symbol<'input>, SymbolDebugInfo<'input>>,
    pub(crate) inlined_functions: Vec<InlinedFunction<'input>>,
}

pub(crate) struct SymbolDebugInfo<'input> {
    compdir: &'input Path,
    directory: Option<&'input OsStr>,
    path_name: &'input OsStr,
    line: u32,
    column: Option<u32>,
    // The name of what this symbol refers to. This is sometimes, but not always the demangled
    // version of the symbol. In particular, when generics are involved, the symbol often doesn't
    // include them, but this does.
    pub(crate) name: Option<DebugName<'input>>,
}

/// A reference pair resulting from one function being inlined into another.
pub(crate) struct InlinedFunction<'input> {
    pub(crate) bin_location: BinLocation,
    pub(crate) from: SymbolAndName<'input>,
    pub(crate) to: SymbolAndName<'input>,
    call_location: CallLocation<'input>,
}

impl<'input> InlinedFunction<'input> {
    pub(crate) fn location(&self) -> Result<SourceLocation> {
        let line = self.call_location.line.ok_or_else(|| {
            anyhow!(
                "Inlined call without line numbers are not supported {} -> {}",
                self.from,
                self.to
            )
        })?;
        let mut path = self.call_location.compdir.to_owned();
        if let Some(dir) = self.call_location.directory {
            path.push(dir);
        }
        if let Some(filename) = self.call_location.filename {
            path.push(filename);
        }
        Ok(SourceLocation::new(path, line, self.call_location.column))
    }
}

impl<'input> DebugArtifacts<'input> {
    pub(crate) fn from_dwarf(dwarf: &Dwarf<EndianSlice<'input, LittleEndian>>) -> Result<Self> {
        let mut scanner = DwarfScanner {
            out: DebugArtifacts::default(),
        };
        scanner.scan(dwarf)?;
        Ok(scanner.out)
    }
}

impl<'input> DwarfScanner<'input> {
    fn scan(&mut self, dwarf: &Dwarf<EndianSlice<'input, LittleEndian>>) -> Result<()> {
        let mut units = dwarf.units();
        while let Some(header) = units.next()? {
            let unit = dwarf.unit(header)?;
            let compdir = path_from_opt_slice(unit.comp_dir);
            if crate::checker::is_in_rust_std(compdir) {
                continue;
            }

            let mut unit_state = UnitState {
                subprogram_namespaces: get_subprogram_namespaces(&unit, dwarf)?,
                dwarf,
                unit,
                frames: Vec::new(),
                compdir,
            };

            let mut entries = unit_state.unit.entries_raw(None)?;
            while !entries.is_empty() {
                let Some(abbrev) = entries.read_abbreviation()? else {
                    // A null entry indicates the end of nesting level.
                    unit_state.frames.pop();
                    continue;
                };
                let tag = abbrev.tag();
                let mut inline_scanner = InlinedFunctionScanner {
                    names: Default::default(),
                    low_pc: None,
                    symbol_start: None,
                    call_location: CallLocation {
                        compdir,
                        directory: None,
                        filename: None,
                        line: None,
                        column: None,
                    },
                };
                let mut symbol_scanner = SymbolDebugInfoScanner::default();
                let mut namespace = None;
                if tag == gimli::DW_TAG_subprogram
                    || tag == gimli::DW_TAG_inlined_subroutine
                    || tag == gimli::DW_TAG_lexical_block
                    || tag == gimli::DW_TAG_variable
                {
                    if tag == gimli::DW_TAG_subprogram || tag == gimli::DW_TAG_variable {
                        inline_scanner.start_top_level();
                    }
                    for spec in abbrev.attributes() {
                        let attr = entries.read_attribute(*spec)?;
                        inline_scanner.handle_attribute(attr, &unit_state)?;
                        symbol_scanner.handle_attribute(attr)?;
                    }
                    if let Some(inlined_function) =
                        inline_scanner.get_inlined_function(&unit_state.frames)
                    {
                        self.out.inlined_functions.push(inlined_function);
                    }
                    if tag == gimli::DW_TAG_subprogram || tag == gimli::DW_TAG_variable {
                        if let Some((symbol, debug_info)) =
                            symbol_scanner.get_debug_info(&unit_state)?
                        {
                            self.out.symbol_debug_info.insert(symbol, debug_info);
                        }
                    }
                } else if tag == gimli::DW_TAG_namespace || tag == gimli::DW_TAG_structure_type {
                    namespace = unit_state.scan_namespace(&mut entries, abbrev.attributes())?;
                } else {
                    entries.skip_attributes(abbrev.attributes())?;
                }
                if abbrev.has_children() {
                    unit_state.frames.push(FrameState {
                        names: inline_scanner.names,
                        namespace,
                        call_location: inline_scanner.call_location,
                    })
                }
            }
        }
        Ok(())
    }
}

/// Parses the DWARF unit and returns a map from offset of each subprogram within the unit to the
/// namespace that subprogram is contained within. We need to do this in a separate pass, since when
/// we encounter inlined functions, they can reference a subprogram that we haven't yet seen and
/// from the offset, we can determine information about the subprogram's attributes, but not about
/// the namespace in which it's contained.
fn get_subprogram_namespaces(
    unit: &Unit<EndianSlice<LittleEndian>, usize>,
    dwarf: &Dwarf<EndianSlice<LittleEndian>>,
) -> Result<FxHashMap<UnitOffset, Namespace>> {
    let mut subprogram_namespaces: FxHashMap<UnitOffset, Namespace> = Default::default();
    let mut stack: Vec<Option<Namespace>> = Vec::new();
    let mut entries = unit.entries_raw(None)?;
    while !entries.is_empty() {
        let unit_offset = entries.next_offset();
        let Some(abbrev) = entries.read_abbreviation()? else {
            stack.pop();
            continue;
        };
        let mut namespace = None;
        let tag = abbrev.tag();
        match tag {
            gimli::DW_TAG_namespace | gimli::DW_TAG_structure_type => {
                let mut name = None;
                for spec in abbrev.attributes() {
                    let attr = entries.read_attribute(*spec)?;
                    if attr.name() == gimli::DW_AT_name {
                        name = Some(dwarf.attr_string(unit, attr.value())?);
                    }
                }
                if let Some(name) = name {
                    let name = name
                        .to_string()
                        .with_context(|| format!("{tag} has non-UTF-8 name"))?;
                    namespace = Some(
                        if let Some(parent_namespace) = stack.last().and_then(|e| e.as_ref()) {
                            parent_namespace.plus(name)
                        } else {
                            Namespace::empty().plus(name)
                        },
                    );
                }
            }
            _ => {
                if abbrev.tag() == gimli::DW_TAG_subprogram {
                    if let Some(parent_namespace) = stack.last().and_then(|e| e.as_ref()) {
                        subprogram_namespaces.insert(unit_offset, parent_namespace.clone());
                    }
                }
                entries.skip_attributes(abbrev.attributes())?;
            }
        }
        if abbrev.has_children() {
            stack.push(namespace);
        }
    }
    Ok(subprogram_namespaces)
}

struct UnitState<'input, 'dwarf> {
    dwarf: &'dwarf Dwarf<EndianSlice<'input, LittleEndian>>,
    frames: Vec<FrameState<'input>>,
    unit: Unit<EndianSlice<'input, LittleEndian>, usize>,
    compdir: &'input Path,
    subprogram_namespaces: FxHashMap<UnitOffset, Namespace>,
}
impl<'input, 'dwarf> UnitState<'input, 'dwarf> {
    fn attr_string(
        &self,
        attr: AttributeValue<EndianSlice<'input, LittleEndian>, usize>,
    ) -> Result<gimli::EndianSlice<'input, LittleEndian>> {
        Ok(self.dwarf.attr_string(&self.unit, attr)?)
    }

    fn get_directory_and_filename(
        &self,
        file_index: AttributeValue<EndianSlice<'input, LittleEndian>, usize>,
    ) -> Result<(Option<&'input OsStr>, &'input OsStr), anyhow::Error> {
        let header = self.line_program_header()?;
        let gimli::AttributeValue::FileIndex(file_index) = file_index else {
            bail!("Expected FileIndex");
        };
        let Some(file) = header.file(file_index) else {
            bail!("Object file contained invalid file index {file_index}");
        };
        let directory = if let Some(directory) = file.directory(header) {
            let directory = self.attr_string(directory)?;
            Some(OsStr::from_bytes(directory.slice()))
        } else {
            None
        };
        let path_name = OsStr::from_bytes(self.attr_string(file.path_name())?.slice());
        Ok((directory, path_name))
    }

    fn line_program_header(
        &self,
    ) -> Result<&gimli::LineProgramHeader<gimli::EndianSlice<'input, LittleEndian>>> {
        let line_program = self
            .unit
            .line_program
            .as_ref()
            .ok_or_else(|| anyhow!("Missing line program"))?;
        Ok(line_program.header())
    }

    fn get_symbol_and_name(
        &self,
        attr: gimli::Attribute<EndianSlice<'input, LittleEndian>>,
        max_depth: u32,
    ) -> Result<SymbolAndName<'input>> {
        let mut names = SymbolAndName::default();
        if let AttributeValue::UnitRef(offset) = attr.value() {
            let mut x = self.unit.entries_raw(Some(offset))?;
            if let Some(abbrev) = x.read_abbreviation()? {
                for spec in abbrev.attributes() {
                    let attr = x.read_attribute(*spec)?;
                    match attr.name() {
                        gimli::DW_AT_specification => {
                            if max_depth == 0 {
                                bail!("Recursion limit reached when resolving inlined functions");
                            }
                            return self.get_symbol_and_name(attr, max_depth - 1);
                        }
                        gimli::DW_AT_linkage_name => {
                            let value = self.attr_string(attr.raw_value())?;
                            names.symbol = Some(Symbol::borrowed(value.slice()));
                        }
                        gimli::DW_AT_name => {
                            let value = self.attr_string(attr.raw_value())?.to_string()?;
                            names.debug_name = Some(DebugName::new(
                                self.subprogram_namespaces
                                    .get(&offset)
                                    .cloned()
                                    .unwrap_or_else(Namespace::empty),
                                value,
                            ));
                        }
                        _ => (),
                    }
                }
            }
        } else {
            bail!("Unsupported abstract_origin type: {:?}", attr.value());
        }
        Ok(names)
    }

    fn scan_namespace(
        &self,
        entries: &mut gimli::EntriesRaw<EndianSlice<'input, LittleEndian>>,
        attributes: &[gimli::AttributeSpecification],
    ) -> Result<Option<Namespace>> {
        // TODO: See if we can reduce duplication between this function and
        // get_subprogram_namespaces.
        let mut name = None;
        for spec in attributes {
            let attr = entries.read_attribute(*spec)?;
            if attr.name() == gimli::DW_AT_name {
                name = Some(self.attr_string(attr.value())?);
            }
        }
        let Some(name) = name else {
            return Ok(None);
        };
        let name = name.to_string().context("Namespace has non-UTF-8 name")?;
        Ok(Some(
            self.frames
                .last()
                .and_then(|f| f.namespace.as_ref())
                .map(|parent_namespace| parent_namespace.plus(name))
                .unwrap_or_else(|| Namespace::top_level(name)),
        ))
    }

    /// Returns the current namespace or the empty namespace if we're not the direct child of a
    /// namespace.
    fn namespace(&self) -> Namespace {
        self.frames
            .last()
            .and_then(|f| f.namespace.as_ref())
            .cloned()
            .unwrap_or_else(Namespace::empty)
    }
}

struct DwarfScanner<'input> {
    out: DebugArtifacts<'input>,
}

impl<'input> SymbolDebugInfo<'input> {
    pub(crate) fn source_location(&self) -> SourceLocation {
        let mut filename = self.compdir.to_owned();
        if let Some(directory) = self.directory {
            filename.push(directory);
        }
        filename.push(self.path_name);
        SourceLocation::new(filename, self.line, self.column)
    }
}

#[derive(Default)]
struct SymbolDebugInfoScanner<'input> {
    name: Option<AttributeValue<EndianSlice<'input, LittleEndian>>>,
    linkage_name: Option<AttributeValue<EndianSlice<'input, LittleEndian>>>,
    line: Option<u32>,
    column: Option<u32>,
    file_index: Option<AttributeValue<EndianSlice<'input, LittleEndian>>>,
}

impl<'input> SymbolDebugInfoScanner<'input> {
    fn handle_attribute(
        &mut self,
        attr: Attribute<EndianSlice<'input, LittleEndian>>,
    ) -> Result<()> {
        match attr.name() {
            gimli::DW_AT_name => {
                self.name = Some(attr.value());
            }
            gimli::DW_AT_linkage_name => {
                self.linkage_name = Some(attr.value());
            }
            gimli::DW_AT_decl_line => {
                self.line = attr.udata_value().map(|v| v as u32);
            }
            gimli::DW_AT_decl_column => {
                self.column = attr.udata_value().map(|v| v as u32);
            }
            gimli::DW_AT_decl_file => {
                self.file_index = Some(attr.value());
            }
            _ => {}
        }
        Ok(())
    }

    fn get_debug_info<'dwarf>(
        self,
        unit_state: &UnitState<'input, 'dwarf>,
    ) -> Result<Option<(Symbol<'input>, SymbolDebugInfo<'input>)>> {
        // When `linkage_name` and `name` would be the same (symbol is not mangled), then
        // `linkage_name` is omitted, so we use `name` as a fallback.
        let linkage_name = self.linkage_name.or(self.name);

        let name = self
            .name
            .map(|attr| unit_state.attr_string(attr))
            .transpose()?
            .map(|n| n.to_string())
            .transpose()?;

        let Some(linkage_name) = linkage_name else {
            return Ok(None);
        };
        let Some(line) = self.line else {
            return Ok(None);
        };
        let Some(file_index) = self.file_index else {
            return Ok(None);
        };
        let symbol = Symbol::borrowed(
            unit_state
                .attr_string(linkage_name)
                .context("symbol invalid")?
                .slice(),
        );
        let (directory, path_name) = unit_state.get_directory_and_filename(file_index)?;
        Ok(Some((
            symbol,
            SymbolDebugInfo {
                name: name.map(|n| DebugName::new(unit_state.namespace(), n)),
                compdir: unit_state.compdir,
                directory,
                path_name,
                line,
                column: self.column,
            },
        )))
    }
}

fn path_from_opt_slice(slice: Option<gimli::EndianSlice<gimli::LittleEndian>>) -> &Path {
    slice
        .map(|dir| Path::new(OsStr::from_bytes(dir.slice())))
        .unwrap_or_else(|| Path::new(""))
}

struct FrameState<'input> {
    names: SymbolAndName<'input>,
    namespace: Option<Namespace>,
    call_location: CallLocation<'input>,
}

struct InlinedFunctionScanner<'input> {
    names: SymbolAndName<'input>,
    low_pc: Option<u64>,
    call_location: CallLocation<'input>,
    /// The address of the start of the current (non-inlined) symbol.
    symbol_start: Option<u64>,
}

#[derive(Clone)]
struct CallLocation<'input> {
    // TODO: Do we need both compdir and directory?
    compdir: &'input Path,
    directory: Option<&'input OsStr>,
    filename: Option<&'input OsStr>,
    line: Option<u32>,
    column: Option<u32>,
}

impl<'input> InlinedFunctionScanner<'input> {
    fn handle_attribute<'dwarf>(
        &mut self,
        attr: Attribute<EndianSlice<'input, LittleEndian>>,
        unit_state: &UnitState<'input, 'dwarf>,
    ) -> Result<()> {
        match attr.name() {
            gimli::DW_AT_linkage_name => {
                let value = unit_state.attr_string(attr.raw_value())?;
                self.names.symbol = Some(Symbol::borrowed(value.slice()));
            }
            gimli::DW_AT_name => {
                let value = unit_state.attr_string(attr.raw_value())?;
                self.names.debug_name =
                    Some(DebugName::new(unit_state.namespace(), value.to_string()?));
            }
            gimli::DW_AT_abstract_origin => {
                self.names = unit_state.get_symbol_and_name(attr, 10)?;
            }
            gimli::DW_AT_call_file => {
                let (directory, filename) = unit_state.get_directory_and_filename(attr.value())?;
                self.call_location.directory = directory;
                self.call_location.filename = Some(filename);
            }
            gimli::DW_AT_call_line => {
                self.call_location.line = attr.udata_value().map(|v| v as u32);
            }
            gimli::DW_AT_call_column => {
                self.call_location.column = attr.udata_value().map(|v| v as u32);
            }
            gimli::DW_AT_low_pc => {
                self.low_pc = unit_state
                    .dwarf
                    .attr_address(&unit_state.unit, attr.value())
                    .context("Unsupported DW_AT_low_pc")?;
                if self.symbol_start.is_none() {
                    self.symbol_start = self.low_pc;
                }
            }
            gimli::DW_AT_ranges => {
                if let Some(ranges_offset) = unit_state
                    .dwarf
                    .attr_ranges_offset(&unit_state.unit, attr.value())?
                {
                    if let Some(first_range) = unit_state
                        .dwarf
                        .ranges(&unit_state.unit, ranges_offset)?
                        .next()?
                    {
                        self.low_pc = Some(first_range.begin);
                    }
                }
            }
            _ => (),
        }
        Ok(())
    }

    fn get_inlined_function(
        &self,
        frames: &[FrameState<'input>],
    ) -> Option<InlinedFunction<'input>> {
        if self.names.debug_name.is_none() && self.names.symbol.is_none() {
            return None;
        }
        let low_pc = self.low_pc?;
        let symbol_start = self.symbol_start?;
        // Functions with DW_AT_low_pc=0 or 1, or absent have been optimised out so can be ignored.
        if low_pc == 0 || low_pc == 1 {
            return None;
        }
        let mut call_location = &self.call_location;
        for frame in frames.iter().rev() {
            if frame.names.symbol.is_some() || frame.names.debug_name.is_some() {
                if frame
                    .names
                    .symbol
                    .as_ref()
                    .map(|s| s.is_look_through())
                    .unwrap_or(false)
                {
                    call_location = &frame.call_location;
                    continue;
                }
                return Some(InlinedFunction {
                    bin_location: BinLocation {
                        address: low_pc,
                        symbol_start,
                    },
                    from: frame.names.clone(),
                    to: self.names.clone(),
                    call_location: call_location.clone(),
                });
            }
        }
        None
    }

    fn start_top_level(&mut self) {
        self.symbol_start = None;
    }
}
