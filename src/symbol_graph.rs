//! This module builds a graph of relationships between symbols and linker sections. Provided code
//! was compiled with one symbol per section, which it should have been, there should be a 1:1
//! relationship between symbols and sections.
//!
//! We also parse the Dwarf debug information to determine what source file each linker section came
//! from.

use crate::checker::Checker;
use crate::checker::Referee;
use crate::checker::SourceLocation;
use crate::checker::UnknownLocation;
use crate::checker::Usage;
use crate::checker::UsageLocation;
use crate::crate_index::CrateIndex;
use crate::problem::Problems;
use crate::section_name::SectionName;
use crate::symbol::Symbol;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use ar::Archive;
use gimli::Dwarf;
use object::Object;
use object::ObjectSection;
use object::ObjectSymbol;
use once_cell::sync::OnceCell;
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt::Display;
use std::fs::File;
use std::io::Read;
use std::ops::Index;
use std::ops::IndexMut;
use std::os::unix::prelude::OsStrExt;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filetype {
    Rlib,
    Other,
}

enum Reference {
    Section(SectionIndex),
    Name(Symbol),
}

#[derive(Default)]
struct SectionInfo {
    name: SectionName,

    /// The object file that this section was contained in.
    defined_in: PathBuf,

    /// Outgoing references from this section.
    references: Vec<Reference>,

    /// The rust source file that defined this section if we were able to determine this from the
    /// debug info.
    source_filename: Option<PathBuf>,

    /// Symbols that this section defines. Generally there should be exactly one, at least with the
    /// compilation settings that we should be using.
    definitions: Vec<Symbol>,
}

#[derive(Default)]
pub(crate) struct SymGraph {
    sections: Vec<SectionInfo>,

    /// The index of the section in which each non-private symbol is defined.
    symbol_to_section: HashMap<Symbol, SectionIndex>,

    /// The index of the section in which each private symbol is defined. Cleared with each object
    /// file that we parse.
    sym_to_local_section: HashMap<Symbol, SectionIndex>,

    /// For each symbol that has two or more definitions, stores the indices of the sections that
    /// defined that symbol.
    duplicate_symbol_section_indexes: HashMap<Symbol, Vec<SectionIndex>>,
}

#[derive(Clone, Copy)]
pub(crate) struct SectionIndex(usize);

impl SymGraph {
    pub(crate) fn process_file(&mut self, filename: &Path) -> Result<()> {
        let mut buffer = Vec::new();
        match Filetype::from_filename(filename) {
            Filetype::Rlib => {
                let mut archive = Archive::new(File::open(filename)?);
                while let Some(entry_result) = archive.next_entry() {
                    let Ok(mut entry) = entry_result else { continue; };
                    buffer.clear();
                    entry.read_to_end(&mut buffer)?;
                    self.process_file_bytes(filename, &buffer)?;
                }
            }
            Filetype::Other => {
                let file_bytes = std::fs::read(filename)?;
                self.process_file_bytes(filename, &file_bytes)?;
            }
        }
        Ok(())
    }

    pub(crate) fn apply_to_checker(
        &self,
        checker: &mut Checker,
        mapping: &CrateIndex,
    ) -> Result<()> {
        for section in &self.sections {
            if section.name.is_empty() {
                // TODO: Determine if it's OK to just ignore this.
                continue;
            }
            let Some(source_filename) = section.source_filename.as_ref() else {
                // TODO: Determine if it's OK to just ignore this.
                continue;
                //bail!("Couldn't determine source filename for section `{}` in `{}`", section.name, section.defined_in.display());
            };
            // Ignore sources from the rust standard library.
            if source_filename.starts_with("/rustc/") {
                continue;
            }
            let crate_name = mapping
                .crate_name_for_path(source_filename)
                .ok_or_else(|| {
                    anyhow!(
                        "Couldn't find crate name for {} referenced from {}",
                        source_filename.display(),
                        section.defined_in.display(),
                    )
                })?;
            let crate_id = checker.crate_id_from_name(crate_name);
            for reference in &section.references {
                if let Some(ref_name) = self.referenced_symbol(reference) {
                    for name_parts in ref_name.parts()? {
                        checker.path_used(crate_id, &name_parts, || {
                            let location = if let Some(filename) = section.source_filename.clone() {
                                UsageLocation::Source(SourceLocation { filename })
                            } else {
                                UsageLocation::Unknown(UnknownLocation {
                                    object_path: section.defined_in.clone(),
                                })
                            };
                            Usage {
                                location,
                                from: section.as_referee(),
                                to: ref_name.clone(),
                            }
                        });
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> Problems {
        let mut multiple_defs = 0;
        let mut sample_section_name = None;
        let mut problems = Problems::default();
        for section in &self.sections {
            if section.definitions.len() > 1 {
                multiple_defs += 1;
                sample_section_name = Some(&section.name);
            }
        }
        if let Some(name) = sample_section_name {
            problems.push(format!(
                "Multiple definitions for {multiple_defs} sections, e.g. {name}"
            ));
        }
        if let Some((dup, _)) = self.duplicate_symbol_section_indexes.iter().next() {
            problems.push(format!(
                "Multiple definitions for {} symbols, e.g. {}",
                self.duplicate_symbol_section_indexes.len(),
                dup
            ));
        }
        problems
    }

    fn referenced_symbol<'a>(&'a self, reference: &'a Reference) -> Option<&'a Symbol> {
        match reference {
            Reference::Section(section_index) => self.sections[*section_index].definitions.first(),
            Reference::Name(symbol) => Some(symbol),
        }
    }

    fn process_file_bytes(&mut self, filename: &Path, file_bytes: &[u8]) -> Result<()> {
        let obj = object::File::parse(file_bytes)
            .with_context(|| format!("Failed to parse {}", filename.display()))?;
        self.process_object_relocations(&obj, filename)?;
        self.process_debug_info(&obj)?;
        for (sym, indexes) in &self.duplicate_symbol_section_indexes {
            println!("Duplicate symbol `{sym}` defined in:");
            for i in indexes {
                println!("  {}", self.sections[i.0].name);
            }
        }
        Ok(())
    }

    fn process_object_relocations(&mut self, obj: &object::File, filename: &Path) -> Result<()> {
        // TODO: Does ignoring all these sections allow hiding stuff? If we assign all references in
        // these sections to "current" crate, can we not ignore them?
        static IGNORED_SECTIONS: OnceCell<HashSet<&str>> = OnceCell::new();
        let ignored_sections = IGNORED_SECTIONS.get_or_init(|| {
            let mut s = HashSet::new();
            s.insert(".eh_frame");
            s.insert(".group");
            s.insert(".note.GNU-stack");
            s.insert(".strtab");
            s.insert(".symtab");
            s.insert(".data.rel.ro");
            s.insert(".debug_info");
            s.insert(".debug_aranges");
            s.insert(".debug_ranges");
            s.insert(".debug_line");

            // If we don't ignore these, then we get duplicate symbol definitions.
            s.insert(".data.DW.ref.rust_eh_personality");
            s.insert(".debug_gdb_scripts");
            s
        });

        let mut section_name_to_index = HashMap::new();
        for section in obj.sections() {
            if let Ok(name) = section.name() {
                if name.starts_with(".rela")
                    || name.starts_with(".data.rel")
                    // TODO: Definitely look into if we can not ignore .rodata.
                    || name.starts_with(".rodata")
                    || ignored_sections.contains(name)
                {
                    continue;
                }
                let index = SectionIndex(self.sections.len());
                section_name_to_index.insert(name.to_owned(), index);
                self.sections.push(SectionInfo::new(filename, name));
            }
        }
        self.sym_to_local_section.clear();
        for sym in obj.symbols() {
            let name = sym.name_bytes().unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let Some(section_name) = section_name_for_symbol(&sym, obj) else { continue };
            let Some(&index) = section_name_to_index.get(&section_name) else { continue };
            self.sections[index].definitions.push(Symbol::new(name));
            if sym.is_local() {
                self.sym_to_local_section.insert(Symbol::new(name), index);
            } else if let Some(old_index) = self.symbol_to_section.insert(Symbol::new(name), index)
            {
                let dup_indexes = self
                    .duplicate_symbol_section_indexes
                    .entry(Symbol::new(name))
                    .or_default();
                dup_indexes.push(index);
                dup_indexes.push(old_index);
            }
        }
        for section in obj.sections() {
            let Ok(section_name) = section.name() else { continue };
            let Some(&section_index) = section_name_to_index.get(section_name) else { continue };
            let section_info = &mut self.sections[section_index];
            for (_offset, rel) in section.relocations() {
                let object::RelocationTarget::Symbol(symbol_index) = rel.target() else { continue };
                let Ok(symbol) = obj.symbol_by_index(symbol_index) else { continue };
                let name = symbol.name_bytes().unwrap_or_default();
                if name.is_empty() {
                    if let Some(section_name) = section_name_for_symbol(&symbol, obj) {
                        if let Some(section_index) =
                            section_name_to_index.get(section_name.as_str())
                        {
                            section_info
                                .references
                                .push(Reference::Section(*section_index));
                        }
                    }
                } else {
                    let symbol = Symbol::new(name);

                    if let Some(local_index) = self.sym_to_local_section.get(&symbol) {
                        section_info
                            .references
                            .push(Reference::Section(*local_index));
                    } else {
                        section_info.references.push(Reference::Name(symbol));
                    }
                }
            }
        }
        Ok(())
    }

    fn process_debug_info(&mut self, obj: &object::File) -> Result<(), anyhow::Error> {
        let owned_dwarf = Dwarf::load(|id| load_section(obj, id))?;
        let dwarf =
            owned_dwarf.borrow(|section| gimli::EndianSlice::new(section, gimli::LittleEndian));
        let mut units = dwarf.units();
        while let Some(header) = units.next()? {
            let unit = dwarf.unit(header)?;
            let compdir = path_from_opt_slice(unit.comp_dir);
            let Some(line_program) = &unit.line_program else { continue };
            let header = line_program.header();

            let mut entries = unit.entries();
            while let Some((_, entry)) = entries.next_dfs()? {
                if entry.tag() != gimli::DW_TAG_subprogram {
                    continue;
                }
                let Ok(Some(attr)) = entry.attr_value(gimli::DW_AT_linkage_name) else {
                        continue
                    };
                let Ok(symbol) = dwarf.attr_string(&unit, attr) else { continue };

                let Ok(Some(gimli::AttributeValue::FileIndex(file_index))) =
                        entry.attr_value(gimli::DW_AT_decl_file) else {
                            continue
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

                let symbol = Symbol::new(symbol.to_vec());
                let Some(&section_id) = self.sym_to_local_section.get(&symbol).or_else(|| self.symbol_to_section.get(&symbol)) else {
                    // TODO: Investigate this
                    //println!("SYM NOT FOUND: {symbol}");
                    continue;
                    //bail!("Debug info references unknown symbol `{symbol}`");
                };
                self.sections[section_id].source_filename = Some(path);
            }
        }
        Ok(())
    }
}

impl Index<SectionIndex> for Vec<SectionInfo> {
    type Output = SectionInfo;

    fn index(&self, index: SectionIndex) -> &Self::Output {
        &self[index.0]
    }
}

impl IndexMut<SectionIndex> for Vec<SectionInfo> {
    fn index_mut(&mut self, index: SectionIndex) -> &mut Self::Output {
        &mut self[index.0]
    }
}

impl Display for SymGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for section in &self.sections {
            write!(f, "{}", section.name)?;
            if let Some(path) = &section.source_filename {
                write!(f, " ({})", path.display())?;
            }
            writeln!(f)?;
            for reference in &section.references {
                match reference {
                    Reference::Section(section_index) => {
                        writeln!(f, "  -> {}", self.sections[*section_index].name)?;
                    }
                    Reference::Name(symbol) => {
                        writeln!(f, "  -> {}", symbol)?;
                    }
                }
            }
        }
        Ok(())
    }
}

impl SectionInfo {
    fn new(defined_in: &Path, name: &str) -> Self {
        Self {
            name: SectionName::new(name.as_bytes()),
            defined_in: defined_in.to_owned(),
            ..Default::default()
        }
    }

    fn as_referee(&self) -> Referee {
        if let Some(sym) = self.definitions.first() {
            Referee::Symbol(sym.clone())
        } else {
            Referee::Section(self.name.clone())
        }
    }
}

fn section_name_for_symbol(symbol: &object::Symbol, obj: &object::File) -> Option<String> {
    symbol
        .section_index()
        .and_then(|section_index| obj.section_by_index(section_index).ok())
        .and_then(|section| section.name().ok().map(|name| name.to_owned()))
}

/// Loads section `id` from `obj`. We return a Cow because it's what gimli expects, but we only ever
/// return an owned Cow because we need to copy the section data so that we can apply relocations to
/// it.
fn load_section(
    obj: &object::File,
    id: gimli::SectionId,
) -> Result<Cow<'static, [u8]>, gimli::Error> {
    let Some(section) = obj.section_by_name(id.name()) else {
        return Ok(Cow::Borrowed([].as_slice()));
    };
    let Ok(data) = section.uncompressed_data() else {
        return Ok(Cow::Borrowed([].as_slice()));
    };
    let mut data = data.into_owned();
    for (offset, rel) in section.relocations() {
        let offset = offset as usize;
        let size = (rel.size() / 8) as usize;
        let mut value = load_var_int(offset, size, &data)?;
        if let object::RelocationKind::Absolute = rel.kind() {
            if rel.has_implicit_addend() {
                value = value.wrapping_add(rel.addend());
            } else {
                value = rel.addend();
            }
        }
        store_var_int(offset, size, &mut data, value)?;
    }
    Ok(Cow::Owned(data))
}

/// Read an integer of `size` bytes at `offset` within `data`. We always read little-endian because
/// we don't support big endian. Value is returned as an i64 since it's the largest type we support.
fn load_var_int(offset: usize, size: usize, data: &[u8]) -> Result<i64, gimli::Error> {
    if offset + size >= data.len() {
        return Err(gimli::Error::InvalidAddressRange);
    }
    let bytes = &data[offset..offset + size];

    Ok(match size {
        4 => i32::from_le_bytes(bytes.try_into().unwrap()) as i64,
        8 => i64::from_le_bytes(bytes.try_into().unwrap()),
        _ => return Err(gimli::Error::UnsupportedFieldSize((offset * 8) as u8)),
    })
}

/// Like `load_var_int`, but stores the supplied value rather than reading it. If `value` is too
/// large to fit in `size` bytes, then wrapping is applied.
fn store_var_int(
    offset: usize,
    size: usize,
    data: &mut [u8],
    value: i64,
) -> Result<(), gimli::Error> {
    if offset + size >= data.len() {
        return Err(gimli::Error::InvalidAddressRange);
    }
    data[offset..offset + size].copy_from_slice(&value.to_le_bytes()[..size]);
    Ok(())
}

fn path_from_opt_slice(slice: Option<gimli::EndianSlice<gimli::LittleEndian>>) -> &Path {
    slice
        .map(|dir| Path::new(OsStr::from_bytes(dir.slice())))
        .unwrap_or_else(|| Path::new(""))
}

impl Filetype {
    fn from_filename(filename: &Path) -> Self {
        let Some(extension) = filename
        .extension() else {
            return Filetype::Other;
        };
        if extension == "rlib" {
            Filetype::Rlib
        } else {
            Filetype::Other
        }
    }
}
