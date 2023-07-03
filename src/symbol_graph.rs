//! This module builds a graph of relationships between symbols and linker sections. Provided code
//! was compiled with one symbol per section, which it should have been, there should be a 1:1
//! relationship between symbols and sections.
//!
//! We also parse the Dwarf debug information to determine what source file each linker section came
//! from.

use crate::checker::Checker;
use crate::checker::Referee;
use crate::checker::SourceLocation;
use crate::checker::Usage;
use crate::problem::ApiUsage;
use crate::problem::ProblemList;
use crate::section_name::SectionName;
use crate::symbol::Symbol;
use crate::Args;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use ar::Archive;
use gimli::Dwarf;
use gimli::EndianSlice;
use gimli::LittleEndian;
use log::info;
use object::Object;
use object::ObjectSection;
use object::ObjectSymbol;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::ops::Index;
use std::ops::IndexMut;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filetype {
    Archive,
    Other,
}

struct Reference {
    target: ReferenceTarget,
    filename: Option<String>,
}

enum ReferenceTarget {
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

    /// Symbols that this section defines. Generally there should be exactly one, at least with the
    /// compilation settings that we should be using.
    definitions: Vec<SymbolInSection>,

    /// Whether this section is reachable by following references from a root (e.g. `main`).
    reachable: bool,
}

struct SymbolInSection {
    symbol: Symbol,
    // Offset of the symbol within the section.
    offset: u64,
}

#[derive(Default)]
struct SymGraph {
    sections: Vec<SectionInfo>,

    /// The index of the section in which each non-private symbol is defined.
    symbol_to_section: HashMap<Symbol, SectionIndex>,

    /// The index of the section in which each private symbol is defined. Cleared with each object
    /// file that we parse.
    sym_to_local_section: HashMap<Symbol, SectionIndex>,

    /// For each symbol that has two or more definitions, stores the indices of the sections that
    /// defined that symbol.
    duplicate_symbol_section_indexes: HashMap<Symbol, Vec<SectionIndex>>,

    /// Whether `compute_reachability` has been called and it has suceeded.
    reachabilty_computed: bool,

    exe: ExeInfo,
}

/// Information derived from a linked binary. Generally an executable, but could also be shared
/// object (so).
#[derive(Default)]
struct ExeInfo {
    symbol_addresses: HashMap<Symbol, u64>,
}

pub(crate) struct GraphOutputs {
    api_usages: Vec<ApiUsage>,

    /// Problems not related to api_usage. These can't be fixed by config changes via the UI, since
    /// once computed, they won't be recomputed.
    base_problems: ProblemList,
}

#[derive(Clone, Copy)]
struct SectionIndex(usize);

pub(crate) fn scan_objects(
    paths: &[PathBuf],
    exe_path: &Path,
    checker: &Checker,
) -> Result<GraphOutputs> {
    let file_bytes = std::fs::read(exe_path)
        .with_context(|| format!("Failed to read `{}`", exe_path.display()))?;
    let obj = object::File::parse(file_bytes.as_slice())
        .with_context(|| format!("Failed to parse {}", exe_path.display()))?;
    let owned_dwarf = Dwarf::load(|id| load_section(&obj, id))?;
    let dwarf = owned_dwarf.borrow(|section| gimli::EndianSlice::new(section, gimli::LittleEndian));
    let ctx = addr2line::Context::from_dwarf(dwarf)
        .with_context(|| format!("Failed to process {}", exe_path.display()))?;

    let mut graph = SymGraph::default();
    graph.exe.load_symbols(&obj)?;
    for path in paths {
        graph
            .process_file(path, &ctx)
            .with_context(|| format!("Failed to process `{}`", path.display()))?;
    }
    graph.compute_reachability(&checker.args)?;
    graph.api_usages(checker)
}

impl GraphOutputs {
    pub(crate) fn problems(&self, checker: &mut Checker) -> Result<ProblemList> {
        let mut problems = self.base_problems.clone();
        for api_usage in &self.api_usages {
            checker.permission_used(api_usage, &mut problems);
        }

        Ok(problems)
    }
}

impl SymGraph {
    fn process_file(
        &mut self,
        filename: &Path,
        ctx: &addr2line::Context<EndianSlice<LittleEndian>>,
    ) -> Result<()> {
        let mut buffer = Vec::new();
        match Filetype::from_filename(filename) {
            Filetype::Archive => {
                let mut archive = Archive::new(File::open(filename)?);
                while let Some(entry_result) = archive.next_entry() {
                    let Ok(mut entry) = entry_result else { continue; };
                    buffer.clear();
                    entry.read_to_end(&mut buffer)?;
                    self.process_file_bytes(filename, &buffer, ctx)?;
                }
            }
            Filetype::Other => {
                let file_bytes = std::fs::read(filename)
                    .with_context(|| format!("Failed to read `{}`", filename.display()))?;
                self.process_file_bytes(filename, &file_bytes, ctx)?;
            }
        }
        Ok(())
    }

    fn compute_reachability(&mut self, args: &Args) -> Result<()> {
        if self.reachabilty_computed {
            return Ok(());
        }
        let start = std::time::Instant::now();
        let mut queue = Vec::with_capacity(100);
        const ROOT_PREFIXES: &[&str] = &[".text.main", ".data.rel.ro.__rustc_proc_macro_decls"];
        queue.extend(
            self.sections
                .iter()
                .enumerate()
                .filter_map(|(index, section)| {
                    if ROOT_PREFIXES
                        .iter()
                        .any(|prefix| section.name.raw_bytes().starts_with(prefix.as_bytes()))
                    {
                        Some(SectionIndex(index))
                    } else {
                        None
                    }
                }),
        );
        if queue.is_empty() {
            if args.verbose_errors {
                println!("Sections names:");
                for section in &self.sections {
                    println!("  {}", section.name);
                }
            }
            bail!("No roots found when computing reachability, but ignore_unreachable is set");
        }
        while let Some(section_index) = queue.pop() {
            if self.sections[section_index.0].reachable {
                // We've already visited this node in the graph.
                continue;
            }
            self.sections[section_index.0].reachable = true;
            let section = &self.sections[section_index.0];
            for reference in &section.references {
                let next_section_index = match &reference.target {
                    ReferenceTarget::Section(section_index) => Some(*section_index),
                    ReferenceTarget::Name(symbol) => self.symbol_to_section.get(symbol).cloned(),
                };
                queue.extend(next_section_index.into_iter());
            }
        }
        if args.print_timing {
            println!(
                "Reachability computation took {}ms",
                start.elapsed().as_millis()
            );
        }
        self.reachabilty_computed = true;
        Ok(())
    }

    fn api_usages(&self, checker: &Checker) -> Result<GraphOutputs> {
        let mut api_usages = Vec::new();
        let mut problems = ProblemList::default();
        if let Some((dup, _)) = self.duplicate_symbol_section_indexes.iter().next() {
            problems.push(format!(
                "Multiple definitions for {} symbols, e.g. {}",
                self.duplicate_symbol_section_indexes.len(),
                dup
            ));
        }
        for section in &self.sections {
            if section.name.is_empty() {
                // TODO: Determine if it's OK to just ignore this.
                info!("Got empty section name");
                continue;
            }
            let reachable = if self.reachabilty_computed {
                Some(section.reachable)
            } else {
                None
            };
            for reference in &section.references {
                let Some(source_filename) = reference.filename.as_ref() else {
                    continue;
                };
                let source_filename = Path::new(source_filename);
                // Ignore sources from the rust standard library and precompiled crates that are bundled
                // with the standard library (e.g. hashbrown).
                if source_filename.starts_with("/rustc/")
                    || source_filename.starts_with("/cargo/registry")
                {
                    continue;
                }
                let crate_names =
                    checker.crate_names_from_source_path(source_filename, &section.defined_in)?;
                for crate_name in crate_names {
                    if let Some(ref_name) = self.referenced_symbol(&reference.target) {
                        for name_parts in ref_name.parts()? {
                            // If a package references another symbol within the same package, ignore
                            // it.
                            if name_parts
                                .first()
                                .map(|name_start| crate_name.as_ref() == name_start)
                                .unwrap_or(false)
                            {
                                continue;
                            }
                            let location = SourceLocation {
                                filename: source_filename.to_owned(),
                            };
                            for permission in checker.apis_for_path(&name_parts) {
                                let mut usages = BTreeMap::new();
                                usages.insert(
                                    permission.clone(),
                                    vec![Usage {
                                        location: location.clone(),
                                        from: section.as_referee(),
                                        to: ref_name.clone(),
                                    }],
                                );
                                api_usages.push(ApiUsage {
                                    crate_name: crate_name.clone(),
                                    usages,
                                    reachable,
                                });
                            }
                        }
                    }
                }
            }
        }
        Ok(GraphOutputs {
            api_usages,
            base_problems: problems,
        })
    }

    fn referenced_symbol<'a>(&'a self, reference: &'a ReferenceTarget) -> Option<&'a Symbol> {
        match reference {
            ReferenceTarget::Section(section_index) => self.sections[*section_index]
                .definitions
                .first()
                .map(|d| &d.symbol),
            ReferenceTarget::Name(symbol) => Some(symbol),
        }
    }

    fn process_file_bytes(
        &mut self,
        filename: &Path,
        file_bytes: &[u8],
        ctx: &addr2line::Context<EndianSlice<LittleEndian>>,
    ) -> Result<()> {
        let obj = object::File::parse(file_bytes)
            .with_context(|| format!("Failed to parse {}", filename.display()))?;
        self.process_object_relocations(&obj, filename, ctx)?;
        for (sym, indexes) in &self.duplicate_symbol_section_indexes {
            println!("Duplicate symbol `{sym}` defined in:");
            for i in indexes {
                println!("  {}", self.sections[i.0].name);
            }
        }
        Ok(())
    }

    fn process_object_relocations(
        &mut self,
        obj: &object::File,
        filename: &Path,
        ctx: &addr2line::Context<EndianSlice<LittleEndian>>,
    ) -> Result<()> {
        let mut section_name_to_index = HashMap::new();
        for section in obj.sections() {
            if let Ok(name) = section.name() {
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
            self.sections[index].definitions.push(SymbolInSection {
                symbol: Symbol::new(name),
                offset: sym.address(),
            });
            if sym.is_local() {
                self.sym_to_local_section.insert(Symbol::new(name), index);
            } else if let Some(old_index) = self.symbol_to_section.insert(Symbol::new(name), index)
            {
                if !(self.is_duplicate_symbol_ok(index, name)) {
                    let dup_indexes = self
                        .duplicate_symbol_section_indexes
                        .entry(Symbol::new(name))
                        .or_default();
                    dup_indexes.push(index);
                    dup_indexes.push(old_index);
                }
            }
        }
        for section in obj.sections() {
            let Ok(section_name) = section.name() else { continue };
            let Some(&section_index) = section_name_to_index.get(section_name) else { continue };
            let section_info = &mut self.sections[section_index];
            let section_start_in_exe =
                section_info
                    .definitions
                    .get(0)
                    .and_then(|first_def_in_section| {
                        self.exe
                            .symbol_addresses
                            .get(&first_def_in_section.symbol)
                            .map(|section_start| section_start - first_def_in_section.offset)
                    });
            let Some(section_start_in_exe) = section_start_in_exe else { continue };
            for (offset, rel) in section.relocations() {
                let location = ctx
                    .find_location(section_start_in_exe + offset)
                    .context("find_location failed")?;
                let source_filename = location.and_then(|l| l.file).map(|f| f.to_owned());
                let object::RelocationTarget::Symbol(symbol_index) = rel.target() else { continue };
                let Ok(symbol) = obj.symbol_by_index(symbol_index) else { continue };
                let name = symbol.name_bytes().unwrap_or_default();
                // TODO: There's a bit of duplication in the following code that needs fixing.
                if name.is_empty() {
                    if let Some(section_name) = section_name_for_symbol(&symbol, obj) {
                        if let Some(section_index) =
                            section_name_to_index.get(section_name.as_str())
                        {
                            section_info.references.push(Reference {
                                target: ReferenceTarget::Section(*section_index),
                                filename: source_filename,
                            });
                        }
                    }
                } else {
                    let symbol = Symbol::new(name);

                    if let Some(local_index) = self.sym_to_local_section.get(&symbol) {
                        section_info.references.push(Reference {
                            target: ReferenceTarget::Section(*local_index),
                            filename: source_filename,
                        });
                    } else {
                        section_info.references.push(Reference {
                            target: ReferenceTarget::Name(symbol),
                            filename: source_filename,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Returns whether it's allowed that we encountered a duplicate symbol `name` in the specified
    /// section.
    fn is_duplicate_symbol_ok(&mut self, index: SectionIndex, name: &[u8]) -> bool {
        &self.sections[index.0].name == ".data.DW.ref.rust_eh_personality"
            && name == b"DW.ref.rust_eh_personality"
    }
}

impl ExeInfo {
    fn load_symbols(&mut self, obj: &object::File) -> Result<()> {
        for symbol in obj.symbols() {
            self.symbol_addresses
                .insert(Symbol::new(symbol.name_bytes()?), symbol.address());
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
            Referee::Symbol(sym.symbol.clone())
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

/// Loads section `id` from `obj`.
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
    // TODO: Now that we're loading binaries rather than object files, we don't apply relocations.
    // We might not need owned data here.
    Ok(Cow::Owned(data.into_owned()))
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
