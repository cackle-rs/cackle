//! This module builds a graph of relationships between symbols and linker sections. Provided code
//! was compiled with one symbol per section, which it should have been, there should be a 1:1
//! relationship between symbols and sections.
//!
//! We also parse the Dwarf debug information to determine what source file each linker section came
//! from.

use self::dwarf::SymbolDebugInfo;
use self::object_file_path::ObjectFilePath;
use crate::checker::ApiUsage;
use crate::checker::Checker;
use crate::config::CrateName;
use crate::config::PermissionName;
use crate::crate_index::CrateSel;
use crate::location::SourceLocation;
use crate::names::Name;
use crate::problem::ApiUsages;
use crate::problem::PossibleExportedApi;
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
use log::debug;
use log::trace;
use object::Object;
use object::ObjectSection;
use object::ObjectSymbol;
use object::RelocationTarget;
use object::SectionIndex;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Display;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

mod dwarf;
pub(crate) mod object_file_path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filetype {
    Archive,
    Other,
}

struct ApiUsageCollector<'input> {
    outputs: ScanOutputs,

    bin: BinInfo<'input>,
    debug_enabled: bool,
}

/// Information derived from a linked binary. Generally an executable, but could also be shared
/// object (so).
struct BinInfo<'input> {
    filename: Arc<Path>,
    symbol_addresses: HashMap<Symbol<'input>, u64>,
    ctx: addr2line::Context<EndianSlice<'input, LittleEndian>>,

    /// Information about each symbol obtained from the debug info.
    symbol_debug_info: HashMap<Symbol<'input>, SymbolDebugInfo<'input>>,
}

#[derive(Default)]
pub(crate) struct ScanOutputs {
    api_usages: Vec<ApiUsages>,

    /// Problems not related to api_usage. These can't be fixed by config changes via the UI, since
    /// once computed, they won't be recomputed.
    base_problems: ProblemList,

    possible_exported_apis: Vec<PossibleExportedApi>,
}

struct ObjectIndex<'obj, 'data> {
    obj: &'obj object::File<'data>,

    section_infos: Vec<SectionInfo<'data>>,
}

#[derive(Clone, Default)]
struct SectionInfo<'data> {
    first_symbol: Option<SymbolInfo<'data>>,
}

#[derive(Clone)]
struct SymbolInfo<'data> {
    /// The first symbol in the section.
    symbol: Symbol<'data>,

    /// The offset of the symbol.
    offset: u64,
}

pub(crate) fn scan_objects(
    paths: &[PathBuf],
    bin_path: &Path,
    checker: &mut Checker,
) -> Result<ScanOutputs> {
    let start = Instant::now();
    let file_bytes = std::fs::read(bin_path)
        .with_context(|| format!("Failed to read `{}`", bin_path.display()))?;
    let obj = object::File::parse(file_bytes.as_slice())
        .with_context(|| format!("Failed to parse {}", bin_path.display()))?;
    let owned_dwarf = Dwarf::load(|id| load_section(&obj, id))?;
    let dwarf = owned_dwarf.borrow(|section| gimli::EndianSlice::new(section, gimli::LittleEndian));
    let symbol_to_locations = dwarf::get_symbol_debug_info(&dwarf)?;
    let ctx = addr2line::Context::from_dwarf(dwarf)
        .with_context(|| format!("Failed to process {}", bin_path.display()))?;
    let start = checker.timings.add_timing(start, "Parse bin");

    let mut collector = ApiUsageCollector {
        outputs: Default::default(),
        bin: BinInfo {
            filename: Arc::from(bin_path),
            symbol_addresses: Default::default(),
            ctx,
            symbol_debug_info: symbol_to_locations,
        },
        debug_enabled: checker.args.debug,
    };
    collector.bin.load_symbols(&obj)?;
    let start = checker.timings.add_timing(start, "Load symbols from bin");
    collector.find_possible_exports(checker);
    let start = checker.timings.add_timing(start, "Find possible exports");
    for path in paths {
        collector
            .process_file(path, checker)
            .with_context(|| format!("Failed to process `{}`", path.display()))?;
    }
    checker.timings.add_timing(start, "Process object files");

    Ok(collector.outputs)
}

impl ScanOutputs {
    pub(crate) fn problems(&self, checker: &mut Checker) -> Result<ProblemList> {
        let mut problems = self.base_problems.clone();
        for api_usage in &self.api_usages {
            checker.permission_used(api_usage, &mut problems);
        }
        checker.possible_exported_api_problems(&self.possible_exported_apis, &mut problems);

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
                    let Ok(mut entry) = entry_result else {
                        continue;
                    };
                    buffer.clear();
                    entry.read_to_end(&mut buffer)?;
                    self.process_object_file_bytes(
                        &ObjectFilePath::in_archive(filename, &entry)?,
                        &buffer,
                        checker,
                    )?;
                }
            }
            Filetype::Other => {
                let file_bytes = std::fs::read(filename)
                    .with_context(|| format!("Failed to read `{}`", filename.display()))?;
                self.process_object_file_bytes(
                    &ObjectFilePath::non_archive(filename),
                    &file_bytes,
                    checker,
                )?;
            }
        }
        Ok(())
    }

    /// Processes an unlinked object file - as opposed to an executable or a shared object, which
    /// has been linked.
    fn process_object_file_bytes(
        &mut self,
        filename: &ObjectFilePath,
        file_bytes: &[u8],
        checker: &Checker,
    ) -> Result<()> {
        debug!("Processing object file {}", filename);

        let obj = object::File::parse(file_bytes)
            .with_context(|| format!("Failed to parse {}", filename))?;
        let object_index = ObjectIndex::new(&obj);
        let mut new_api_usages: HashMap<_, Vec<ApiUsages>> = HashMap::new();
        for section in obj.sections() {
            let section_name = section.name().unwrap_or("");
            let Some(first_sym_info) = object_index.first_symbol(&section) else {
                debug!("Skipping section `{section_name}` due to lack of debug info");
                continue;
            };
            let Some(symbol_address_in_bin) = self.bin.symbol_addresses.get(&first_sym_info.symbol)
            else {
                debug!(
                    "Skipping section `{}` because symbol `{}` doesn't appear in exe/so",
                    section_name, first_sym_info.symbol
                );
                continue;
            };
            let Some(debug_info) = self.bin.symbol_debug_info.get(&first_sym_info.symbol) else {
                continue;
            };
            let Some(from_name) = debug_info.name.as_ref() else {
                continue;
            };

            // Compute what APIs are used by the from-function. Any references made by that function
            // that would trigger the same APIs are then ignored. Basically, we attribute API usages
            // to the outermost usage of an API.
            let mut from_apis = HashSet::new();
            for name in crate::names::split_names(from_name).into_iter() {
                from_apis.extend(checker.apis_for_name(&name).into_iter());
            }
            for name in first_sym_info.symbol.names()? {
                from_apis.extend(checker.apis_for_name(&name).into_iter());
            }

            for (offset, rel) in section.relocations() {
                let location = self
                    .bin
                    .find_location(symbol_address_in_bin + offset - first_sym_info.offset)?
                    .unwrap_or_else(|| debug_info.source_location());
                // Ignore references that come from code in the rust standard library.
                if location.is_in_rust_std() {
                    continue;
                }
                let crate_names =
                    checker.crate_names_from_source_path(location.filename(), filename)?;

                for target_symbol in object_index.target_symbols(&rel)? {
                    trace!("{} -> {target_symbol}", first_sym_info.symbol);

                    let target_symbol_names = self.bin.names_from_symbol(&target_symbol)?;
                    for crate_sel in &crate_names {
                        let crate_name = CrateName::from(crate_sel);
                        for (name, name_source) in &target_symbol_names {
                            // If a package references another symbol within the same package,
                            // ignore it.
                            if name
                                .parts
                                .first()
                                .map(|name_start| crate_name.as_ref() == name_start)
                                .unwrap_or(false)
                            {
                                continue;
                            }
                            for permission in checker.apis_for_name(name) {
                                if from_apis.contains(&permission) {
                                    continue;
                                }
                                let debug_data = self.debug_enabled.then(|| UsageDebugData {
                                    bin_path: self.bin.filename.clone(),
                                    object_file_path: filename.clone(),
                                    section_name: section_name.to_owned(),
                                });
                                let mut usages = BTreeMap::new();
                                usages.insert(
                                    permission.clone(),
                                    vec![ApiUsage {
                                        source_location: location.clone(),
                                        from: first_sym_info.symbol.to_heap(),
                                        to: name.clone(),
                                        to_symbol: target_symbol.to_heap(),
                                        to_source: name_source.to_owned(),
                                        debug_data,
                                    }],
                                );
                                let api_usage = ApiUsages {
                                    crate_sel: crate_sel.clone(),
                                    usages,
                                };
                                new_api_usages
                                    .entry(api_usage.deduplication_key())
                                    .or_default()
                                    .push(api_usage);
                            }
                        }
                    }
                }
            }
        }
        // New API usages are grouped by their deduplication key, which doesn't include the target
        // symbol. We then output only the API usage with the shortest target symbol.
        for api_usages in new_api_usages.into_values() {
            if let Some(shortest_target_usage) = api_usages
                .into_iter()
                .min_by_key(|u| u.first_usage().unwrap().to_symbol.len())
            {
                self.outputs.api_usages.push(shortest_target_usage);
            }
        }
        Ok(())
    }

    fn find_possible_exports(&mut self, checker: &Checker) {
        let api_names: HashMap<&str, &PermissionName> = checker
            .config
            .apis
            .keys()
            .map(|n| (n.name.as_ref(), n))
            .collect();
        let mut found = HashSet::new();
        for (symbol, debug_info) in &self.bin.symbol_debug_info {
            let Some(module_name) = symbol.module_name() else {
                continue;
            };
            let Some(permission_name) = api_names.get(module_name) else {
                continue;
            };
            let location = debug_info.source_location();
            for crate_sel in checker
                .opt_crate_names_from_source_path(location.filename())
                .as_deref()
                .unwrap_or_default()
            {
                // APIs can only be exported from the primary crate in a package.
                let CrateSel::Primary(pkg_id) = crate_sel else {
                    continue;
                };
                if found.insert((pkg_id.clone(), permission_name)) {
                    self.outputs
                        .possible_exported_apis
                        .push(PossibleExportedApi {
                            pkg_id: pkg_id.to_owned(),
                            api: PermissionName::clone(permission_name),
                            symbol: symbol.to_heap(),
                        });
                }
            }
        }
    }
}

impl<'obj, 'data> ObjectIndex<'obj, 'data> {
    fn new(obj: &'obj object::File<'data>) -> Self {
        let max_section_index = obj.sections().map(|s| s.index().0).max().unwrap_or(0);
        let mut section_infos = vec![SectionInfo::default(); max_section_index + 1];
        for obj_symbol in obj.symbols() {
            let name = obj_symbol.name_bytes().unwrap_or_default();
            if name.is_empty() || !obj_symbol.is_definition() {
                continue;
            }
            let Some(section_index) = obj_symbol.section_index() else {
                continue;
            };
            let section_info = &mut section_infos[section_index.0];
            let symbol_is_first_in_section = section_info
                .first_symbol
                .as_ref()
                .map(|existing| obj_symbol.address() < existing.offset)
                .unwrap_or(true);
            if symbol_is_first_in_section {
                section_info.first_symbol = Some(SymbolInfo {
                    symbol: Symbol::borrowed(name),
                    offset: obj_symbol.address(),
                });
            }
        }
        Self { obj, section_infos }
    }

    /// Returns the symbol or symbols that `rel` refers to. If `rel` refers to a section that
    /// doesn't define a non-local symbol at address 0, then all outgoing references from that
    /// section will be included and so on recursively.
    fn target_symbols(&self, rel: &object::Relocation) -> Result<Vec<Symbol<'data>>> {
        let mut symbols_out = Vec::new();
        self.add_target_symbols(rel, &mut symbols_out, &mut HashSet::new())?;
        Ok(symbols_out)
    }

    fn add_target_symbols(
        &self,
        rel: &object::Relocation,
        symbols_out: &mut Vec<Symbol<'data>>,
        visited: &mut HashSet<SectionIndex>,
    ) -> Result<()> {
        match self.get_symbol_or_section(rel.target())? {
            SymbolOrSection::Symbol(symbol) => {
                symbols_out.push(symbol);
            }
            SymbolOrSection::Section(section_index) => {
                if !visited.insert(section_index) {
                    // We've already visited this section.
                    return Ok(());
                }
                let section = self.obj.section_by_index(section_index)?;
                for (_, rel) in section.relocations() {
                    self.add_target_symbols(&rel, symbols_out, visited)?;
                }
            }
        }
        Ok(())
    }

    /// Returns either symbol or the section index for a relocation target, giving preference to the
    /// symbol.
    fn get_symbol_or_section(&self, target_in: RelocationTarget) -> Result<SymbolOrSection<'data>> {
        let section_index = match target_in {
            RelocationTarget::Symbol(symbol_index) => {
                let Ok(symbol) = self.obj.symbol_by_index(symbol_index) else {
                    bail!("Invalid symbol index in object file");
                };
                let name = symbol.name_bytes().unwrap_or_default();
                if !name.is_empty() {
                    return Ok(SymbolOrSection::Symbol(Symbol::borrowed(name).to_heap()));
                }
                symbol.section_index().ok_or_else(|| {
                    anyhow!("Relocation target has empty name an no section index")
                })?
            }
            RelocationTarget::Section(_) => todo!(),
            _ => bail!("Unsupported relocation kind {target_in:?}"),
        };
        let section_info = &self
            .section_infos
            .get(section_index.0)
            .ok_or_else(|| anyhow!("Unnamed symbol has invalid section index"))?;
        if let Some(first_symbol_info) = section_info.first_symbol.as_ref() {
            return Ok(SymbolOrSection::Symbol(first_symbol_info.symbol.clone()));
        }
        Ok(SymbolOrSection::Section(section_index))
    }

    /// Returns information about the first symbol in the section.
    fn first_symbol(&self, section: &object::Section) -> Option<&SymbolInfo<'data>> {
        self.section_infos
            .get(section.index().0)
            .and_then(|section_info| section_info.first_symbol.as_ref())
    }
}

enum SymbolOrSection<'data> {
    Symbol(Symbol<'data>),
    Section(SectionIndex),
}

impl<'input> BinInfo<'input> {
    fn load_symbols(&mut self, obj: &object::File) -> Result<()> {
        for symbol in obj.symbols() {
            self.symbol_addresses.insert(
                Symbol::borrowed(symbol.name_bytes()?).to_heap(),
                symbol.address(),
            );
        }
        Ok(())
    }

    /// Returns names present either in `symbol` or in the debug info for `symbol`. Generally the
    /// former is fully qualified, while the latter contains generics, so we need both.
    fn names_from_symbol<'symbol>(
        &self,
        symbol: &Symbol<'symbol>,
    ) -> Result<Vec<(Name, NameSource<'symbol>)>> {
        let mut names: Vec<_> = symbol
            .names()?
            .into_iter()
            .map(|name| (name, NameSource::Symbol(symbol.clone())))
            .collect();
        if let Some(target_symbol_debug) = self.symbol_debug_info.get(symbol) {
            if let Some(debug_name) = target_symbol_debug.name {
                let debug_name = Arc::from(debug_name);
                // This is O(n^2) in the number of names, but we expect N to be in the range 1..3
                // and rarely more than 5, so using a hashmap or similar seems like overkill.
                for name in crate::names::split_names(&debug_name) {
                    if !names.iter().any(|(n, _)| n == &name) {
                        names.push((name, NameSource::DebugName(debug_name.clone())));
                    }
                }
            }
        }
        Ok(names)
    }

    fn find_location(&self, offset: u64) -> Result<Option<SourceLocation>> {
        use addr2line::Location;

        let Some(location) = self
            .ctx
            .find_location(offset)
            .context("find_location failed")?
        else {
            return Ok(None);
        };
        let Location {
            file: Some(file),
            line: Some(line),
            column,
        } = location
        else {
            return Ok(None);
        };
        Ok(Some(SourceLocation::new(Path::new(file), line, column)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NameSource<'symbol> {
    Symbol(Symbol<'symbol>),
    DebugName(Arc<str>),
}

impl<'symbol> NameSource<'symbol> {
    fn to_owned(&self) -> NameSource<'static> {
        match self {
            NameSource::Symbol(symbol) => NameSource::Symbol(symbol.to_heap()),
            NameSource::DebugName(name) => NameSource::DebugName(name.clone()),
        }
    }
}

impl<'symbol> Display for NameSource<'symbol> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NameSource::Symbol(symbol) => symbol.fmt(f),
            NameSource::DebugName(debug_name) => debug_name.fmt(f),
        }
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
        let Some(extension) = filename.extension() else {
            return Filetype::Other;
        };
        if extension == "rlib" || extension == ".a" {
            Filetype::Archive
        } else {
            Filetype::Other
        }
    }
}

/// Additional information that might be useful for debugging. Only available when --debug is
/// passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsageDebugData {
    bin_path: Arc<Path>,
    object_file_path: ObjectFilePath,
    section_name: String,
}
