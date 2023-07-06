use crate::checker::SourceLocation;
use crate::symbol::Symbol;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use gimli::Dwarf;
use gimli::EndianSlice;
use gimli::LittleEndian;
use gimli::Reader;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::prelude::OsStrExt;
use std::path::Path;

pub(super) fn get_symbol_to_locations(
    dwarf: &Dwarf<EndianSlice<LittleEndian>>,
) -> Result<HashMap<Symbol, SourceLocation>> {
    let mut locations = HashMap::new();
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
            let Some(name) = entry.attr_value(gimli::DW_AT_linkage_name)? else {
                continue;
            };
            let Some(line) = entry
                .attr_value(gimli::DW_AT_decl_line)?
                .and_then(|v| v.udata_value())
            else {
                continue;
            };
            let column = entry
                .attr_value(gimli::DW_AT_decl_column)?
                .and_then(|v| v.udata_value())
                .map(|v| v as u32);
            let symbol = Symbol::new(
                dwarf
                    .attr_string(&unit, name)
                    .context("symbol invalid")?
                    .to_slice()?,
            );
            let Ok(Some(gimli::AttributeValue::FileIndex(file_index))) =
                entry.attr_value(gimli::DW_AT_decl_file)
            else {
                continue;
            };
            let Some(file) = header.file(file_index) else {
                bail!("Object file contained invalid file index {file_index}");
            };
            let mut path = compdir.to_owned();
            if let Some(directory) = file.directory(header) {
                let directory = dwarf.attr_string(&unit, directory)?;
                path.push(OsStr::from_bytes(directory.as_ref()));
            }
            path.push(OsStr::from_bytes(
                dwarf.attr_string(&unit, file.path_name())?.as_ref(),
            ));

            locations.insert(
                symbol,
                SourceLocation {
                    filename: path,
                    line: line as u32,
                    column,
                },
            );
        }
    }
    Ok(locations)
}

fn path_from_opt_slice(slice: Option<gimli::EndianSlice<gimli::LittleEndian>>) -> &Path {
    slice
        .map(|dir| Path::new(OsStr::from_bytes(dir.slice())))
        .unwrap_or_else(|| Path::new(""))
}
