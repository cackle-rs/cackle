use crate::location::SourceLocation;
use crate::symbol::Symbol;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use gimli::Attribute;
use gimli::AttributeValue;
use gimli::Dwarf;
use gimli::EndianSlice;
use gimli::IncompleteLineProgram;
use gimli::LittleEndian;
use gimli::Unit;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::prelude::OsStrExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct DebugArtifacts<'input> {
    pub(crate) symbol_debug_info: HashMap<Symbol<'input>, SymbolDebugInfo<'input>>,
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
    pub(crate) from_symbol: Symbol<'input>,
    pub(crate) to_symbol: Symbol<'input>,
    pub(crate) location: SourceLocation,
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
        let mut frames: Vec<FrameState> = Vec::new();
        while let Some(header) = units.next()? {
            let unit = dwarf.unit(header)?;
            let compdir = path_from_opt_slice(unit.comp_dir);
            if crate::checker::is_in_rust_std(compdir) {
                continue;
            }
            let Some(line_program) = &unit.line_program else {
                continue;
            };
            let mut entries = unit.entries_raw(None)?;
            while !entries.is_empty() {
                let Some(abbrev) = entries.read_abbreviation()? else {
                    // A null entry indicates the end of nesting level.
                    frames.pop();
                    continue;
                };
                let mut inline_scanner = InlinedFunctionScanner::default();
                let mut symbol_scanner = SymbolDebugInfoScanner::default();
                for spec in abbrev.attributes() {
                    let attr = entries.read_attribute(*spec)?;
                    inline_scanner.handle_attribute(attr, &unit, dwarf, line_program, compdir)?;
                    symbol_scanner.handle_attribute(attr)?;
                }
                let symbol = inline_scanner.symbol.clone();
                if let Some(inlined_function) = inline_scanner.get_inlined_function(&frames) {
                    self.out.inlined_functions.push(inlined_function);
                }
                if let Some((symbol, debug_info)) =
                    symbol_scanner.get_debug_info(line_program.header(), dwarf, &unit, compdir)?
                {
                    self.out.symbol_debug_info.insert(symbol, debug_info);
                }
                if abbrev.has_children() {
                    frames.push(FrameState { symbol })
                }
            }
        }
        Ok(())
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

    fn get_debug_info(
        self,
        header: &gimli::LineProgramHeader<EndianSlice<'input, LittleEndian>, usize>,
        dwarf: &Dwarf<EndianSlice<'input, LittleEndian>>,
        unit: &gimli::Unit<EndianSlice<'input, LittleEndian>, usize>,
        compdir: &'input Path,
    ) -> Result<Option<(Symbol<'input>, SymbolDebugInfo<'input>)>> {
        // When `linkage_name` and `name` would be the same (symbol is not mangled), then
        // `linkage_name` is omitted, so we use `name` as a fallback.
        let linkage_name = self.linkage_name.or(self.name);

        let name = self
            .name
            .map(|attr| dwarf.attr_string(unit, attr))
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
            dwarf
                .attr_string(unit, linkage_name)
                .context("symbol invalid")?
                .slice(),
        );
        let (directory, path_name) = get_directory_and_filename(file_index, header, dwarf, unit)?;
        Ok(Some((
            symbol,
            SymbolDebugInfo {
                name,
                compdir,
                directory,
                path_name,
                line,
                column: self.column,
            },
        )))
    }
}

fn get_directory_and_filename<'input>(
    file_index: AttributeValue<EndianSlice<'input, LittleEndian>, usize>,
    header: &gimli::LineProgramHeader<EndianSlice<'input, LittleEndian>, usize>,
    dwarf: &Dwarf<EndianSlice<'input, LittleEndian>>,
    unit: &gimli::Unit<EndianSlice<'input, LittleEndian>, usize>,
) -> Result<(Option<&'input OsStr>, &'input OsStr), anyhow::Error> {
    let gimli::AttributeValue::FileIndex(file_index) = file_index else {
        bail!("Expected FileIndex");
    };
    let Some(file) = header.file(file_index) else {
        bail!("Object file contained invalid file index {file_index}");
    };
    let directory = if let Some(directory) = file.directory(header) {
        let directory = dwarf.attr_string(unit, directory)?;
        Some(OsStr::from_bytes(directory.slice()))
    } else {
        None
    };
    let path_name = OsStr::from_bytes(dwarf.attr_string(unit, file.path_name())?.slice());
    Ok((directory, path_name))
}

fn path_from_opt_slice(slice: Option<gimli::EndianSlice<gimli::LittleEndian>>) -> &Path {
    slice
        .map(|dir| Path::new(OsStr::from_bytes(dir.slice())))
        .unwrap_or_else(|| Path::new(""))
}

struct FrameState<'input> {
    symbol: Option<Symbol<'input>>,
}

#[derive(Default)]
struct InlinedFunctionScanner<'input> {
    symbol: Option<Symbol<'input>>,
    call_file: Option<PathBuf>,
    call_line: Option<u32>,
    call_column: Option<u32>,
}

impl<'input> InlinedFunctionScanner<'input> {
    fn handle_attribute(
        &mut self,
        attr: Attribute<EndianSlice<'input, LittleEndian>>,
        unit: &Unit<EndianSlice<'input, LittleEndian>>,
        dwarf: &Dwarf<EndianSlice<'input, LittleEndian>>,
        line_program: &IncompleteLineProgram<EndianSlice<'input, LittleEndian>>,
        compdir: &Path,
    ) -> Result<()> {
        match attr.name() {
            gimli::DW_AT_linkage_name => {
                let linkage_name = dwarf.attr_string(unit, attr.raw_value())?;
                self.symbol = Some(Symbol::borrowed(linkage_name.slice()));
            }
            gimli::DW_AT_abstract_origin => {
                self.symbol = get_symbol(attr, unit, dwarf)?;
            }
            gimli::DW_AT_call_file => {
                let (directory, filename) =
                    get_directory_and_filename(attr.value(), line_program.header(), dwarf, unit)?;
                if let Some(directory) = directory {
                    self.call_file = Some(compdir.join(directory).join(filename));
                } else {
                    self.call_file = Some(compdir.join(filename).to_owned());
                }
            }
            gimli::DW_AT_call_line => {
                self.call_line = attr.udata_value().map(|v| v as u32);
            }
            gimli::DW_AT_call_column => {
                self.call_column = attr.udata_value().map(|v| v as u32);
            }
            _ => (),
        }
        Ok(())
    }

    fn get_inlined_function(
        self,
        frames: &[FrameState<'input>],
    ) -> Option<InlinedFunction<'input>> {
        let Some(symbol) = self.symbol else {
            return None;
        };
        let Some(file) = self.call_file else {
            return None;
        };
        let Some(line) = self.call_line else {
            return None;
        };
        for frame in frames.iter().rev() {
            if let Some(prev_sym) = frame.symbol.as_ref() {
                let location =
                    SourceLocation::new(Arc::from(file.as_path()), line, self.call_column);
                return Some(InlinedFunction {
                    from_symbol: prev_sym.clone(),
                    to_symbol: symbol.clone(),
                    location,
                });
            }
        }
        None
    }
}

fn get_symbol<'input>(
    attr: gimli::Attribute<EndianSlice<'input, LittleEndian>>,
    unit: &gimli::Unit<EndianSlice<'input, LittleEndian>, usize>,
    dwarf: &Dwarf<EndianSlice<'input, LittleEndian>>,
) -> Result<Option<Symbol<'input>>> {
    if let AttributeValue::UnitRef(offset) = attr.value() {
        let mut x = unit.entries_raw(Some(offset))?;
        if let Some(abbrev) = x.read_abbreviation()? {
            for spec in abbrev.attributes() {
                let attr = x.read_attribute(*spec)?;
                if let gimli::DW_AT_linkage_name = attr.name() {
                    let linkage_name = dwarf.attr_string(unit, attr.raw_value())?;
                    return Ok(Some(Symbol::borrowed(linkage_name.slice())));
                }
            }
        }
    }
    Ok(None)
}
