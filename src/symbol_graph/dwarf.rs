use crate::location::SourceLocation;
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
    pub(crate) name: Option<&'input str>,
}

/// A reference pair resulting from one function being inlined into another.
pub(crate) struct InlinedFunction<'input> {
    pub(crate) from: SymbolAndName<'input>,
    pub(crate) to: SymbolAndName<'input>,
    call_location: CallLocation<'input>,
    pub(crate) low_pc: Option<u64>,
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
                    call_location: CallLocation {
                        compdir,
                        directory: None,
                        filename: None,
                        line: None,
                        column: None,
                    },
                };
                let mut symbol_scanner = SymbolDebugInfoScanner::default();
                if tag == gimli::DW_TAG_subprogram
                    || tag == gimli::DW_TAG_inlined_subroutine
                    || tag == gimli::DW_TAG_lexical_block
                    || tag == gimli::DW_TAG_variable
                {
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
                    if let Some((symbol, debug_info)) =
                        symbol_scanner.get_debug_info(&unit_state)?
                    {
                        self.out.symbol_debug_info.insert(symbol, debug_info);
                    }
                } else {
                    entries.skip_attributes(abbrev.attributes())?;
                }
                if abbrev.has_children() {
                    unit_state.frames.push(FrameState {
                        names: inline_scanner.names,
                    })
                }
            }
        }
        Ok(())
    }
}

struct UnitState<'input, 'dwarf> {
    dwarf: &'dwarf Dwarf<EndianSlice<'input, LittleEndian>>,
    frames: Vec<FrameState<'input>>,
    unit: Unit<EndianSlice<'input, LittleEndian>, usize>,
    compdir: &'input Path,
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
                            let value = self.attr_string(attr.raw_value())?;
                            names.debug_name = Some(value.to_string()?);
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
                name,
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
}

struct InlinedFunctionScanner<'input> {
    names: SymbolAndName<'input>,
    low_pc: Option<u64>,
    call_location: CallLocation<'input>,
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
                self.names.debug_name = Some(value.to_string()?);
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
        // Functions with DW_AT_low_pc=0 or 1, or absent have been optimised out so can be ignored.
        match self.low_pc {
            None | Some(0) | Some(1) => return None,
            _ => {}
        }
        for frame in frames.iter().rev() {
            if frame.names.symbol.is_some() || frame.names.debug_name.is_some() {
                return Some(InlinedFunction {
                    from: frame.names.clone(),
                    to: self.names.clone(),
                    call_location: self.call_location.clone(),
                    low_pc: self.low_pc,
                });
            }
        }
        None
    }
}
