//! This module builds a graph of relationships between symbols and linker sections. Provided code
//! was compiled with one symbol per section, which it should have been, there should be a 1:1
//! relationship between symbols and sections.
//!
//! We also parse the Dwarf debug information to determine what source file each linker section came
//! from.

use crate::checker::Checker;
use crate::checker::SourceLocation;
use crate::checker::Usage;
use crate::problem::ApiUsage;
use crate::problem::ProblemList;
use crate::symbol::Symbol;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use ar::Archive;
use gimli::Dwarf;
use gimli::EndianSlice;
use gimli::LittleEndian;
use object::Object;
use object::ObjectSection;
use object::ObjectSymbol;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filetype {
    Archive,
    Other,
}

struct ApiUsageCollector<'input> {
    outputs: ScanOutputs,

    exe: ExeInfo<'input>,
}

/// Information derived from a linked binary. Generally an executable, but could also be shared
/// object (so).
struct ExeInfo<'input> {
    symbol_addresses: HashMap<Symbol, u64>,
    ctx: addr2line::Context<EndianSlice<'input, LittleEndian>>,
}

#[derive(Default)]
pub(crate) struct ScanOutputs {
    api_usages: Vec<ApiUsage>,

    /// Problems not related to api_usage. These can't be fixed by config changes via the UI, since
    /// once computed, they won't be recomputed.
    base_problems: ProblemList,
}

struct ObjectIndex<'obj, 'data> {
    obj: &'obj object::File<'data>,

    /// For each section, stores a symbol defined at the start of that section, if any.
    section_index_to_symbol: Vec<Option<Symbol>>,
}

pub(crate) fn scan_objects(
    paths: &[PathBuf],
    exe_path: &Path,
    checker: &Checker,
) -> Result<ScanOutputs> {
    let file_bytes = std::fs::read(exe_path)
        .with_context(|| format!("Failed to read `{}`", exe_path.display()))?;
    let obj = object::File::parse(file_bytes.as_slice())
        .with_context(|| format!("Failed to parse {}", exe_path.display()))?;
    let owned_dwarf = Dwarf::load(|id| load_section(&obj, id))?;
    let dwarf = owned_dwarf.borrow(|section| gimli::EndianSlice::new(section, gimli::LittleEndian));
    let ctx = addr2line::Context::from_dwarf(dwarf)
        .with_context(|| format!("Failed to process {}", exe_path.display()))?;

    let mut collector = ApiUsageCollector {
        outputs: Default::default(),
        exe: ExeInfo {
            symbol_addresses: Default::default(),
            ctx,
        },
    };
    collector.exe.load_symbols(&obj)?;
    for path in paths {
        collector
            .process_file(path, checker)
            .with_context(|| format!("Failed to process `{}`", path.display()))?;
    }

    Ok(collector.outputs)
}

impl ScanOutputs {
    pub(crate) fn problems(&self, checker: &mut Checker) -> Result<ProblemList> {
        let mut problems = self.base_problems.clone();
        for api_usage in &self.api_usages {
            checker.permission_used(api_usage, &mut problems);
        }

        Ok(problems)
    }
}

impl<'input> ApiUsageCollector<'input> {
    fn process_file(&mut self, filename: &Path, checker: &Checker) -> Result<()> {
        let mut buffer = Vec::new();
        match Filetype::from_filename(filename) {
            Filetype::Archive => {
                let mut archive = Archive::new(File::open(filename)?);
                while let Some(entry_result) = archive.next_entry() {
                    let Ok(mut entry) = entry_result else { continue; };
                    buffer.clear();
                    entry.read_to_end(&mut buffer)?;
                    self.process_object_file_bytes(filename, &buffer, checker)?;
                }
            }
            Filetype::Other => {
                let file_bytes = std::fs::read(filename)
                    .with_context(|| format!("Failed to read `{}`", filename.display()))?;
                self.process_object_file_bytes(filename, &file_bytes, checker)?;
            }
        }
        Ok(())
    }

    /// Processes an unlinked object file - as opposed to an executable or a shared object, which
    /// has been linked.
    fn process_object_file_bytes(
        &mut self,
        filename: &Path,
        file_bytes: &[u8],
        checker: &Checker,
    ) -> Result<()> {
        let obj = object::File::parse(file_bytes)
            .with_context(|| format!("Failed to parse {}", filename.display()))?;
        let object_index = ObjectIndex::new(&obj);
        for section in obj.sections() {
            let Some(section_start_symbol) = object_index
                .section_index_to_symbol
                .get(section.index().0)
                .and_then(Option::as_ref) else {
                    continue;
                };
            let Some(section_start_in_exe) = self.exe.symbol_addresses.get(section_start_symbol) else {
                continue;
            };
            for (offset, rel) in section.relocations() {
                let Some(location) = self.exe.find_location(section_start_in_exe + offset)? else {
                    // Code generated by the compiler don't have any source location associated with
                    // them and can be safely ignored.
                    continue;
                };
                let Some(target_symbol) = object_index.target_symbol(&rel)? else {
                    continue;
                };
                // Ignore references that come from code in the rust standard library.
                if location.is_in_rust_std() {
                    continue;
                }

                let crate_names =
                    checker.crate_names_from_source_path(&location.filename, filename)?;
                let mut api_usages = Vec::new();
                for crate_name in crate_names {
                    for name_parts in target_symbol.parts()? {
                        // If a package references another symbol within the same package, ignore
                        // it.
                        if name_parts
                            .first()
                            .map(|name_start| crate_name.as_ref() == name_start)
                            .unwrap_or(false)
                        {
                            continue;
                        }
                        for permission in checker.apis_for_path(&name_parts) {
                            let mut usages = BTreeMap::new();
                            usages.insert(
                                permission.clone(),
                                vec![Usage {
                                    location: location.clone(),
                                    from: section_start_symbol.clone(),
                                    to: target_symbol.clone(),
                                }],
                            );
                            api_usages.push(ApiUsage {
                                crate_name: crate_name.clone(),
                                usages,
                            });
                        }
                    }
                }
                self.outputs.api_usages.append(&mut api_usages);
            }
        }
        Ok(())
    }
}

impl<'obj, 'data> ObjectIndex<'obj, 'data> {
    fn new(obj: &'obj object::File<'data>) -> Self {
        let max_section_index = obj.sections().map(|s| s.index().0).max().unwrap_or(0);
        let mut first_symbol_by_section = vec![None; max_section_index + 1];
        for symbol in obj.symbols() {
            let name = symbol.name_bytes().unwrap_or_default();
            if symbol.address() != 0 || name.is_empty() {
                continue;
            }
            let Some(section_index) = symbol.section_index() else {
                continue;
            };
            first_symbol_by_section[section_index.0] = Some(Symbol::new(name));
        }
        Self {
            obj,
            section_index_to_symbol: first_symbol_by_section,
        }
    }

    fn target_symbol(&self, rel: &object::Relocation) -> Result<Option<Symbol>> {
        let object::RelocationTarget::Symbol(symbol_index) = rel.target() else { bail!("Unsupported relocation kind"); };
        let Ok(symbol) = self.obj.symbol_by_index(symbol_index) else { bail!("Invalid symbol index in object file"); };
        let name = symbol.name_bytes().unwrap_or_default();
        if !name.is_empty() {
            return Ok(Some(Symbol::new(name)));
        }
        let Some(section_index) = symbol.section_index() else {
            bail!("Relocation target has empty name and no section index");
        };
        Ok(self
            .section_index_to_symbol
            .get(section_index.0)
            .ok_or_else(|| anyhow!("Unnamed symbol has invalid section index"))?
            .clone())
    }
}

impl<'input> ExeInfo<'input> {
    fn load_symbols(&mut self, obj: &object::File) -> Result<()> {
        for symbol in obj.symbols() {
            self.symbol_addresses
                .insert(Symbol::new(symbol.name_bytes()?), symbol.address());
        }
        Ok(())
    }

    fn find_location(&self, offset: u64) -> Result<Option<SourceLocation>> {
        let location = self
            .ctx
            .find_location(offset)
            .context("find_location failed")?;
        let filename = location.and_then(|l| l.file);
        let Some(filename) = filename else {
            return Ok(None);
        };
        Ok(Some(SourceLocation {
            filename: PathBuf::from(filename),
        }))
    }
}

/// Loads section `id` from `obj`.
fn load_section<'data>(
    obj: &object::File<'data>,
    id: gimli::SectionId,
) -> Result<Cow<'data, [u8]>, gimli::Error> {
    let Some(section) = obj.section_by_name(id.name()) else {
        return Ok(Cow::Borrowed([].as_slice()));
    };
    let Ok(data) = section.uncompressed_data() else {
        return Ok(Cow::Borrowed([].as_slice()));
    };
    Ok(data)
}

impl Filetype {
    fn from_filename(filename: &Path) -> Self {
        let Some(extension) = filename
        .extension() else {
            return Filetype::Other;
        };
        if extension == "rlib" || extension == ".a" {
            Filetype::Archive
        } else {
            Filetype::Other
        }
    }
}
