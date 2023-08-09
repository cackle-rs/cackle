use crate::location::SourceLocation;
use crate::symbol::Symbol;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use gimli::AttributeValue;
use gimli::Dwarf;
use gimli::EndianSlice;
use gimli::LittleEndian;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::prelude::OsStrExt;
use std::path::Path;
use std::sync::Arc;

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

pub(super) fn get_symbol_debug_info<'input>(
    dwarf: &Dwarf<EndianSlice<'input, LittleEndian>>,
) -> Result<HashMap<Symbol<'input>, SymbolDebugInfo<'input>>> {
    let mut output: HashMap<Symbol, SymbolDebugInfo> = HashMap::new();
    let mut units = dwarf.units();
    while let Some(unit_header) = units.next()? {
        let unit = dwarf.unit(unit_header)?;
        let Some(line_program) = &unit.line_program else {
            continue;
        };
        let header = line_program.header();
        let compdir = path_from_opt_slice(unit.comp_dir);
        let mut entries = unit.entries_raw(None)?;
        while !entries.is_empty() {
            let Some(abbrev) = entries.read_abbreviation()? else {
                continue;
            };
            let mut name = None;
            let mut linkage_name = None;
            let mut line = None;
            let mut column = None;
            let mut file_index = None;
            for spec in abbrev.attributes() {
                let attr = entries.read_attribute(*spec)?;
                match attr.name() {
                    gimli::DW_AT_name => {
                        name = Some(attr.value());
                    }
                    gimli::DW_AT_linkage_name => {
                        linkage_name = Some(attr.value());
                    }
                    gimli::DW_AT_decl_line => {
                        line = attr.udata_value().map(|v| v as u32);
                    }
                    gimli::DW_AT_decl_column => {
                        column = attr.udata_value().map(|v| v as u32);
                    }
                    gimli::DW_AT_decl_file => {
                        file_index = Some(attr.value());
                    }
                    _ => {}
                }
            }

            // When `linkage_name` and `name` would be the same (symbol is not mangled), then
            // `linkage_name` is omitted, so we use `name` as a fallback.
            let linkage_name = linkage_name.or(name);

            let name = name
                .map(|attr| dwarf.attr_string(&unit, attr))
                .transpose()?
                .map(|n| n.to_string())
                .transpose()?;

            let Some(linkage_name) = linkage_name else {
                continue;
            };
            let Some(line) = line else { continue };
            let Some(file_index) = file_index else {
                continue;
            };
            let symbol = Symbol::borrowed(
                dwarf
                    .attr_string(&unit, linkage_name)
                    .context("symbol invalid")?
                    .slice(),
            );
            let (directory, path_name) =
                get_directory_and_filename(file_index, header, dwarf, &unit)?;

            output.insert(
                symbol,
                SymbolDebugInfo {
                    name,
                    compdir,
                    directory,
                    path_name,
                    line,
                    column,
                },
            );
        }
    }
    Ok(output)
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

/// Invokes `emit_ref_pair` for each pair of symbols where the second symbol is a function that was
/// inlined into the first. Also includes the source location where the inlining occurred. i.e. the
/// location in the first function that referenced the second.
pub(crate) fn find_inlined_functions(
    dwarf: &Dwarf<EndianSlice<LittleEndian>>,
    mut emit_ref_pair: impl FnMut(&Symbol, &Symbol, SourceLocation),
) -> Result<()> {
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
            let mut symbol = None;
            let mut file = None;
            let mut line = None;
            let mut column = None;
            match abbrev.tag() {
                gimli::DW_TAG_subprogram => {
                    for spec in abbrev.attributes() {
                        let attr = entries.read_attribute(*spec)?;
                        if let gimli::DW_AT_linkage_name = attr.name() {
                            let linkage_name = dwarf.attr_string(&unit, attr.raw_value())?;
                            symbol = Some(Symbol::borrowed(linkage_name.slice()));
                        }
                    }
                }
                gimli::DW_TAG_inlined_subroutine => {
                    for spec in abbrev.attributes() {
                        let attr = entries.read_attribute(*spec)?;
                        match attr.name() {
                            gimli::DW_AT_abstract_origin => {
                                symbol = get_symbol(attr, &unit, dwarf)?;
                            }
                            gimli::DW_AT_call_file => {
                                let (directory, filename) = get_directory_and_filename(
                                    attr.value(),
                                    line_program.header(),
                                    dwarf,
                                    &unit,
                                )?;
                                if let Some(directory) = directory {
                                    file = Some(compdir.join(directory).join(filename));
                                } else {
                                    file = Some(compdir.join(filename).to_owned());
                                }
                            }
                            gimli::DW_AT_call_line => {
                                line = attr.udata_value().map(|v| v as u32);
                            }
                            gimli::DW_AT_call_column => {
                                column = attr.udata_value().map(|v| v as u32);
                            }
                            _ => (),
                        }
                    }
                }
                _ => {
                    entries.skip_attributes(abbrev.attributes())?;
                }
            }
            if let (Some(symbol), Some(file), Some(line)) = (symbol.as_ref(), &file, &line) {
                for frame in frames.iter().rev() {
                    if let Some(prev_sym) = frame.symbol.as_ref() {
                        let location =
                            SourceLocation::new(Arc::from(file.as_path()), *line, column);
                        emit_ref_pair(prev_sym, symbol, location);
                        break;
                    }
                }
            }
            if abbrev.has_children() {
                frames.push(FrameState { symbol })
            }
        }
    }
    Ok(())
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
