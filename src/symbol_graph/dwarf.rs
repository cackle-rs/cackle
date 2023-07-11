use crate::checker::SourceLocation;
use crate::symbol::Symbol;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use gimli::Dwarf;
use gimli::EndianSlice;
use gimli::LittleEndian;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::prelude::OsStrExt;
use std::path::Path;

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
        SourceLocation {
            filename,
            line: self.line,
            column: self.column,
        }
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
        let mut entries = unit.entries();
        while let Some((_, entry)) = entries.next_dfs()? {
            let name = entry
                .attr_value(gimli::DW_AT_name)?
                .map(|name| dwarf.attr_string(&unit, name))
                .transpose()?
                .map(|name| name.to_string())
                .transpose()?;
            // When `linkage_name` and `name` would be the same (symbol is not mangled), then
            // `linkage_name` is omitted, so we use `name` as a fallback.
            let Some(linkage_name) = entry
                .attr_value(gimli::DW_AT_linkage_name)?
                .or_else(|| entry.attr_value(gimli::DW_AT_name).ok().flatten())
            else {
                continue;
            };
            let Some(line) = entry
                .attr_value(gimli::DW_AT_decl_line)?
                .and_then(|v| v.udata_value())
                .map(|v| v as u32)
            else {
                continue;
            };
            let column = entry
                .attr_value(gimli::DW_AT_decl_column)?
                .and_then(|v| v.udata_value())
                .map(|v| v as u32);
            let symbol = Symbol::borrowed(
                dwarf
                    .attr_string(&unit, linkage_name)
                    .context("symbol invalid")?
                    .slice(),
            );
            let Ok(Some(gimli::AttributeValue::FileIndex(file_index))) =
                entry.attr_value(gimli::DW_AT_decl_file)
            else {
                continue;
            };
            let Some(file) = header.file(file_index) else {
                bail!("Object file contained invalid file index {file_index}");
            };
            let directory = if let Some(directory) = file.directory(header) {
                let directory = dwarf.attr_string(&unit, directory)?;
                Some(OsStr::from_bytes(directory.slice()))
            } else {
                None
            };
            let path_name = OsStr::from_bytes(dwarf.attr_string(&unit, file.path_name())?.slice());

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

fn path_from_opt_slice(slice: Option<gimli::EndianSlice<gimli::LittleEndian>>) -> &Path {
    slice
        .map(|dir| Path::new(OsStr::from_bytes(dir.slice())))
        .unwrap_or_else(|| Path::new(""))
}
