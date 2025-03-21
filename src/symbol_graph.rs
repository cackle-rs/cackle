//! This module builds a graph of relationships between symbols and linker sections. Provided code
//! was compiled with one symbol per section, which it should have been, there should be a 1:1
//! relationship between symbols and sections.
//!
//! We also parse the Dwarf debug information to determine what source file each linker section came
//! from.

use self::backtrace::Backtracer;
use self::dwarf::SymbolDebugInfo;
use self::object_file_path::ObjectFilePath;
use crate::checker::ApiUsage;
use crate::checker::BinLocation;
use crate::checker::Checker;
use crate::config::permissions::PermissionScope;
use crate::config::ApiConfig;
use crate::config::ApiName;
use crate::crate_index::CrateSel;
use crate::crate_index::PackageId;
use crate::link_info::LinkInfo;
use crate::location::SourceLocation;
use crate::names::DebugName;
use crate::names::Name;
use crate::names::SymbolAndName;
use crate::names::SymbolOrDebugName;
use crate::problem::ApiUsages;
use crate::problem::PossibleExportedApi;
use crate::problem::ProblemList;
use crate::symbol::Symbol;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use ar::Archive;
use fxhash::FxHashMap;
use fxhash::FxHashSet;
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
use std::fmt::Display;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

pub(crate) mod backtrace;
mod dwarf;
pub(crate) mod object_file_path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filetype {
    Archive,
    Other,
}

struct ApiUsageCollector<'input, 'backtracer> {
    outputs: ScanOutputs,
    backtracer: Option<&'backtracer mut Backtracer>,

    bin: BinInfo<'input>,
    debug_enabled: bool,
    new_api_usages: FxHashMap<ApiUsageGroupKey, Vec<SingleApiUsage>>,
}

struct SingleApiUsage {
    pkg_id: PackageId,
    scope: PermissionScope,
    api: ApiName,
    usage: ApiUsage,
}

/// Information derived from a linked binary. Generally an executable, but could also be shared
/// object (so).
struct BinInfo<'input> {
    filename: Arc<Path>,
    crate_sel: CrateSel,
    symbol_addresses: FxHashMap<Symbol<'input>, u64>,
    /// Symbols that we've already determined have no APIs. This is an optimisation that lets us
    /// skip these symbols when we see them again.
    symbol_has_no_apis: FxHashMap<Symbol<'input>, bool>,

    /// Information about each symbol obtained from the debug info.
    symbol_debug_info: FxHashMap<Symbol<'input>, SymbolDebugInfo<'input>>,
}

#[derive(Default)]
pub(crate) struct ScanOutputs {
    api_usages: FxHashMap<(PackageId, ApiName), ApiUsages>,

    /// Problems not related to api_usage. These can't be fixed by config changes via the UI, since
    /// once computed, they won't be recomputed.
    base_problems: ProblemList,

    possible_exported_apis: Vec<PossibleExportedApi>,

    /// The API definitions used to produce these outputs. Used to determine if we need to recompute
    /// API usages.
    pub(crate) apis: BTreeMap<ApiName, ApiConfig>,
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
    link_info: &LinkInfo,
    checker: &mut Checker,
) -> Result<(ScanOutputs, Option<Backtracer>)> {
    log::info!("Scanning {}", link_info.output_file.display());
    let start = Instant::now();
    let file_bytes = std::fs::read(&link_info.output_file)
        .with_context(|| format!("Failed to read `{}`", link_info.output_file.display()))?;
    checker.timings.add_timing(start, "Read bin file");

    // Backtraces require that we keep a bunch of stuff around, which uses up memory, so we only do
    // it if the UI is active and if we haven't explicitly disabled backtraces.
    let backtraces = !checker.args.no_backtrace && !checker.args.no_ui;
    let mut backtracer = backtraces.then(|| Backtracer::new(checker.sysroot.clone()));
    let outputs =
        scan_object_with_bin_bytes(&file_bytes, checker, backtracer.as_mut(), link_info, paths)?;

    if let Some(b) = backtracer.as_mut() {
        b.provide_bin_bytes(file_bytes);
    }
    Ok((outputs, backtracer))
}

fn scan_object_with_bin_bytes(
    bin_file_bytes: &Vec<u8>,
    checker: &mut Checker,
    backtracer: Option<&mut Backtracer>,
    link_info: &LinkInfo,
    paths: &[PathBuf],
) -> Result<ScanOutputs> {
    let start = Instant::now();
    let obj = object::File::parse(bin_file_bytes.as_slice())
        .with_context(|| format!("Failed to parse {}", link_info.output_file.display()))?;
    let owned_dwarf = Dwarf::load(|id| load_section(&obj, id))?;
    let dwarf = owned_dwarf.borrow(|section| gimli::EndianSlice::new(section, gimli::LittleEndian));
    let start = checker.timings.add_timing(start, "Parse bin");
    let debug_artifacts =
        dwarf::DebugArtifacts::from_dwarf(&dwarf, checker).with_context(|| {
            format!(
                "Failed while processing debug info for `{}`",
                link_info.output_file.display()
            )
        })?;
    let start = checker.timings.add_timing(start, "Read debug artifacts");
    let ctx = addr2line::Context::from_dwarf(dwarf).with_context(|| {
        format!(
            "Failed in addr2line for `{}`",
            link_info.output_file.display()
        )
    })?;
    let start = checker.timings.add_timing(start, "Build addr2line context");
    let no_api_symbol_hashes = debug_artifacts
        .symbol_debug_info
        .keys()
        .map(|symbol| (symbol.clone(), false))
        .collect();
    let mut collector = ApiUsageCollector {
        outputs: Default::default(),
        backtracer,
        bin: BinInfo {
            filename: link_info.output_file.clone(),
            crate_sel: link_info.crate_sel.clone(),
            symbol_addresses: Default::default(),
            symbol_debug_info: debug_artifacts.symbol_debug_info,
            symbol_has_no_apis: no_api_symbol_hashes,
        },
        debug_enabled: checker.args.debug,
        new_api_usages: FxHashMap::default(),
    };
    collector.bin.load_symbols(&obj)?;
    let start = checker.timings.add_timing(start, "Load symbols from bin");
    for f in debug_artifacts.inlined_functions {
        let from = Node {
            names: f.from,
            location_fetcher: LocationFetcher::InlinedFunction(&f.call_location),
        };
        let debug_data = if checker.args.debug {
            Some(UsageDebugData::Inlined(InlinedDebugData::from_offset(
                Some(f.bin_location.address),
                &ctx,
            )?))
        } else {
            None
        };
        collector.process_reference(
            f.bin_location,
            None,
            &from,
            &f.to,
            checker,
            debug_data.as_ref(),
        )?;
    }
    let start = checker
        .timings
        .add_timing(start, "Process inlined references");
    collector.find_possible_exports(checker);
    let start = checker.timings.add_timing(start, "Find possible exports");
    for path in paths {
        collector
            .process_file(path, checker, &ctx)
            .with_context(|| format!("Failed to process `{}`", path.display()))?;
    }
    collector.emit_shortest_api_usages();
    checker.timings.add_timing(start, "Process object files");
    Ok(collector.outputs)
}

impl ScanOutputs {
    pub(crate) fn problems(&self, checker: &mut Checker) -> Result<ProblemList> {
        let mut problems: ProblemList = self.base_problems.clone();
        for api_usages in self.api_usages.values() {
            checker.api_used(api_usages, &mut problems)?;
        }
        checker.possible_exported_api_problems(&self.possible_exported_apis, &mut problems);

        Ok(problems)
    }
}

impl<'input> ApiUsageCollector<'input, '_> {
    fn process_file(
        &mut self,
        filename: &Path,
        checker: &Checker,
        ctx: &addr2line::Context<EndianSlice<'input, LittleEndian>>,
    ) -> Result<()> {
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
                    let object_file_path = ObjectFilePath::in_archive(filename, &entry)?;
                    self.process_object_file_bytes(&object_file_path, &buffer, checker, ctx)
                        .with_context(|| format!("Failed to process {object_file_path}"))?;
                }
            }
            Filetype::Other => {
                let file_bytes = std::fs::read(filename)
                    .with_context(|| format!("Failed to read `{}`", filename.display()))?;
                let object_file_path = ObjectFilePath::non_archive(filename);
                self.process_object_file_bytes(&object_file_path, &file_bytes, checker, ctx)
                    .with_context(|| format!("Failed to process {object_file_path}"))?;
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
        ctx: &addr2line::Context<EndianSlice<'input, LittleEndian>>,
    ) -> Result<()> {
        debug!("Processing object file {}", filename);

        let obj = object::File::parse(file_bytes).context("Failed to parse object file")?;
        let object_index = ObjectIndex::new(&obj);
        for section in obj.sections() {
            let section_name = section.name().unwrap_or("");
            let Some(first_sym_info) = object_index.first_symbol(&section) else {
                debug!("Skipping section `{section_name}` due to lack of debug info");
                continue;
            };
            let Some(symbol_address_in_bin) = self
                .bin
                .symbol_addresses
                .get(&first_sym_info.symbol)
                .cloned()
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
            let fallback_source_location = debug_info.source_location();
            let debug_data = self.debug_enabled.then(|| {
                UsageDebugData::Relocation(RelocationDebugData {
                    bin_path: self.bin.filename.clone(),
                    object_file_path: filename.clone(),
                    section_name: section_name.to_owned(),
                })
            });

            for (offset, rel) in section.relocations() {
                let mut target_symbols = Vec::new();
                let rel = &rel;
                object_index.add_target_symbols(
                    rel,
                    &mut target_symbols,
                    &mut FxHashSet::default(),
                    &self.bin.symbol_addresses,
                )?;

                // Use debug info to determine the function that the reference originated from.
                let offset_in_bin = symbol_address_in_bin + offset - first_sym_info.offset;
                let mut frames = ctx.find_frames(offset_in_bin).skip_all_loads()?;
                let (frame_fn_name, frame_location) = frames
                    .next()?
                    .map(|frame| (frame.function, frame.location))
                    .unwrap_or((None, None));
                let location_fetcher = LocationFetcher::FrameWithFallback {
                    frame_location,
                    fallback: &fallback_source_location,
                };
                let frame_symbol = frame_fn_name
                    .as_ref()
                    .map(|fn_name| Symbol::borrowed(&fn_name.name));
                let bin_location = BinLocation {
                    address: offset_in_bin,
                    symbol_start: symbol_address_in_bin,
                };

                let from_symbol = frame_symbol.as_ref().unwrap_or(&first_sym_info.symbol);
                let from = Node {
                    names: self.bin.get_symbol_and_name(from_symbol),
                    location_fetcher,
                };
                let mut non_inlined_from = None;
                if frame_symbol.as_ref() != Some(&first_sym_info.symbol) {
                    non_inlined_from = Some(Node {
                        names: self.bin.get_symbol_and_name(&first_sym_info.symbol),
                        location_fetcher: LocationFetcher::AlreadyResolved(
                            &fallback_source_location,
                        ),
                    });
                }
                for target_symbol in target_symbols {
                    if let Some(target_address) = self.bin.symbol_addresses.get(&target_symbol) {
                        if let Some(b) = self.backtracer.as_mut() {
                            b.add_reference(bin_location, *target_address);
                        }
                    }
                    let target = self.bin.get_symbol_and_name(&target_symbol);
                    self.process_reference(
                        bin_location,
                        non_inlined_from.as_ref(),
                        &from,
                        &target,
                        checker,
                        debug_data.as_ref(),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn process_reference(
        &mut self,
        bin_location: BinLocation,
        non_inlined_from: Option<&Node>,
        from: &Node,
        target: &SymbolAndName,
        checker: &Checker,
        debug_data: Option<&UsageDebugData>,
    ) -> Result<(), anyhow::Error> {
        trace!("{} -> {target}", from.names);

        let mut from_apis = FxHashSet::default();
        self.bin
            .names_and_apis_do(&from.names, checker, |_, _, apis| {
                from_apis.extend(apis.iter());
                Ok(())
            })?;
        let mut lazy_location = None;
        let mut lazy_crate_names = None;
        let bin_path = self.bin.filename.clone();
        let bin_sel = self.bin.crate_sel.clone();
        self.bin
            .names_and_apis_do(target, checker, |name, name_source, apis| {
                // For the majority of references we expect no APIs to match. We defer computation
                // of a source location and crate names until we know that an API matched.
                if lazy_location.is_none() {
                    lazy_location = Some(from.location_fetcher.location()?);
                }
                let location = lazy_location.as_ref().unwrap();
                if lazy_crate_names.is_none() {
                    lazy_crate_names = Some(checker.pkg_ids_from_source_path(location.filename())?);
                }
                let crate_names = lazy_crate_names.as_ref().unwrap();

                for pkg_id in crate_names.as_ref() {
                    // If a package references another symbol within the same package,
                    // ignore it.
                    // TODO: This should be use the crate name form (i.e. with underscores, not
                    // hyphens).
                    if name.starts_with(pkg_id.name_str()) {
                        continue;
                    }
                    for api in apis {
                        if from_apis.contains(&api) {
                            continue;
                        }
                        let outer_location = non_inlined_from
                            .map(|n| n.location_fetcher.location())
                            .transpose()?;
                        let api_usage = SingleApiUsage {
                            pkg_id: pkg_id.clone(),
                            scope: PermissionScope::determine(pkg_id, &bin_sel),
                            api: api.clone(),
                            usage: ApiUsage {
                                bin_location,
                                bin_path: bin_path.clone(),
                                permission_scope: PermissionScope::determine(pkg_id, &bin_sel),
                                source_location: location.clone(),
                                outer_location,
                                from: from.names.symbol_or_debug_name()?,
                                to: target.symbol_or_debug_name()?,
                                to_name: name.clone(),
                                to_source: name_source.to_owned(),
                                debug_data: debug_data.cloned(),
                            },
                        };
                        self.new_api_usages
                            .entry(api_usage.group_key())
                            .or_default()
                            .push(api_usage);
                    }
                }
                Ok(())
            })?;
        Ok(())
    }

    fn emit_shortest_api_usages(&mut self) {
        // New API usages are grouped by their deduplication key, which doesn't include the target
        // symbol. We then output only the API usage with the shortest target symbol.
        for api_usages in std::mem::take(&mut self.new_api_usages).into_values() {
            if let Some(shortest_target_usage) =
                api_usages
                    .into_iter()
                    .min_by_key(|u| match &u.usage.to_source {
                        NameSource::Symbol(sym) => sym.len(),
                        NameSource::DebugName(debug_name) => debug_name.name.len(),
                    })
            {
                self.outputs
                    .api_usages
                    .entry((
                        shortest_target_usage.pkg_id.clone(),
                        shortest_target_usage.api.clone(),
                    ))
                    .or_insert_with(|| ApiUsages {
                        pkg_id: shortest_target_usage.pkg_id.clone(),
                        scope: shortest_target_usage.scope,
                        api_name: shortest_target_usage.api.clone(),
                        usages: Default::default(),
                    })
                    .usages
                    .push(shortest_target_usage.usage);
            }
        }
    }

    fn find_possible_exports(&mut self, checker: &Checker) {
        let api_names: FxHashMap<&str, &ApiName> = checker
            .config
            .raw
            .apis
            .keys()
            .map(|n| (n.name.as_ref(), n))
            .collect();
        let mut found = FxHashSet::default();
        for (symbol, debug_info) in &self.bin.symbol_debug_info {
            let Some(module_name) = symbol.module_name() else {
                continue;
            };
            let Some(api_name) = api_names.get(module_name) else {
                continue;
            };
            let location = debug_info.source_location();
            for pkg_id in checker
                .opt_pkg_ids_from_source_path(location.filename())
                .unwrap_or_else(|| Cow::Owned(Vec::new()))
                .as_ref()
            {
                if found.insert((pkg_id.clone(), api_name)) {
                    // Macros can sometimes result in symbols being attributed to lower-level
                    // crates, so we only consider exported APIs that start with the crate name we
                    // expect for the package.
                    if symbol.crate_name() != Some(pkg_id.crate_name().as_ref()) {
                        continue;
                    }
                    self.outputs
                        .possible_exported_apis
                        .push(PossibleExportedApi {
                            pkg_id: pkg_id.to_owned(),
                            api: ApiName::clone(api_name),
                            symbol: symbol.to_heap(),
                        });
                }
            }
        }
    }
}

struct Node<'a> {
    names: SymbolAndName<'a>,
    location_fetcher: LocationFetcher<'a>,
}

enum LocationFetcher<'a> {
    FrameWithFallback {
        frame_location: Option<addr2line::Location<'a>>,
        fallback: &'a SourceLocation,
    },
    InlinedFunction(&'a dwarf::CallLocation<'a>),
    AlreadyResolved(&'a SourceLocation),
}

impl LocationFetcher<'_> {
    fn location(&self) -> Result<SourceLocation> {
        match self {
            LocationFetcher::FrameWithFallback {
                frame_location,
                fallback,
            } => Ok(frame_location
                .as_ref()
                .and_then(|l| l.try_into().ok())
                .unwrap_or_else(|| (*fallback).clone())),
            LocationFetcher::InlinedFunction(call_location) => call_location.location(),
            LocationFetcher::AlreadyResolved(location) => Ok((*location).clone()),
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

    /// Adds the symbol or symbols that `rel` refers to into `symbols_out`. If `rel` refers to a
    /// section that doesn't define a non-local symbol at address 0, then all outgoing references
    /// from that section will be included and so on recursively.
    fn add_target_symbols(
        &self,
        rel: &object::Relocation,
        symbols_out: &mut Vec<Symbol<'data>>,
        visited: &mut FxHashSet<SectionIndex>,
        bin_symbols: &FxHashMap<Symbol, u64>,
    ) -> Result<()> {
        match self.get_symbol_or_section(rel.target(), bin_symbols)? {
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
                    self.add_target_symbols(&rel, symbols_out, visited, bin_symbols)?;
                }
            }
        }
        Ok(())
    }

    /// Returns either symbol or the section index for a relocation target, giving preference to the
    /// symbol.
    fn get_symbol_or_section(
        &self,
        target_in: RelocationTarget,
        bin_symbols: &FxHashMap<Symbol, u64>,
    ) -> Result<SymbolOrSection<'data>> {
        let section_index = match target_in {
            RelocationTarget::Symbol(symbol_index) => {
                let Ok(symbol) = self.obj.symbol_by_index(symbol_index) else {
                    bail!("Invalid symbol index in object file");
                };
                let name = symbol.name_bytes().unwrap_or_default();
                if !name.is_empty() {
                    let sym = Symbol::borrowed(name);
                    if bin_symbols.contains_key(&sym) || symbol.section_index().is_none() {
                        return Ok(SymbolOrSection::Symbol(sym));
                    }
                }
                symbol.section_index().ok_or_else(|| {
                    anyhow!("Relocation target has empty name and no section index")
                })?
            }
            _ => bail!("Unsupported relocation kind {target_in:?}"),
        };
        let section_info = &self
            .section_infos
            .get(section_index.0)
            .ok_or_else(|| anyhow!("Unnamed symbol has invalid section index"))?;
        if let Some(first_symbol_info) = section_info.first_symbol.as_ref() {
            if bin_symbols.contains_key(&first_symbol_info.symbol) {
                return Ok(SymbolOrSection::Symbol(first_symbol_info.symbol.clone()));
            }
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

impl<'symbol, 'input: 'symbol> BinInfo<'input> {
    fn load_symbols(&mut self, obj: &object::File) -> Result<()> {
        for sym in obj.symbols() {
            let symbol = &Symbol::borrowed(sym.name_bytes()?);
            if !symbol.is_look_through() {
                self.symbol_addresses
                    .insert(symbol.to_heap(), sym.address());
            }
        }
        Ok(())
    }

    fn get_symbol_and_name(&self, symbol: &Symbol<'symbol>) -> SymbolAndName<'symbol> {
        let mut result = SymbolAndName {
            symbol: Some(symbol.clone()),
            ..SymbolAndName::default()
        };
        if let Some(symbol_debug) = self.symbol_debug_info.get(symbol) {
            result.debug_name = symbol_debug.name.clone()
        }
        result
    }
}

impl TryFrom<&addr2line::Location<'_>> for SourceLocation {
    type Error = ();
    fn try_from(value: &addr2line::Location) -> std::result::Result<Self, ()> {
        let addr2line::Location {
            file: Some(file),
            line: Some(line),
            column,
        } = value
        else {
            return Err(());
        };
        Ok(SourceLocation::new(Path::new(file), *line, *column))
    }
}

impl BinInfo<'_> {
    /// Runs `callback` for each name in `symbol` or in the name obtained for the debug information
    /// for `symbol`. Also supplies information about the name source and a set of APIs that match
    /// the name.
    fn names_and_apis_do<'checker>(
        &mut self,
        symbol_and_name: &SymbolAndName,
        checker: &'checker Checker,
        mut callback: impl FnMut(Name, NameSource, &'checker FxHashSet<ApiName>) -> Result<()>,
    ) -> Result<()> {
        // If we've previously observed that this symbol has no APIs associated with it, then skip
        // it.
        if symbol_and_name
            .symbol
            .as_ref()
            .and_then(|symbol| self.symbol_has_no_apis.get(symbol))
            .cloned()
            .unwrap_or(false)
        {
            return Ok(());
        }
        let mut got_apis = false;
        if let Some(debug_name) = symbol_and_name.debug_name.as_ref() {
            let mut it = debug_name.names_iterator();
            while let Some((parts, name)) = it
                .next_name()
                .with_context(|| format!("Failed to parse debug name `{debug_name}`"))?
            {
                let apis = checker.apis_for_name_iterator(parts);
                if !apis.is_empty() {
                    got_apis = true;
                    (callback)(
                        name.create_name()?,
                        NameSource::DebugName(debug_name.to_heap()),
                        apis,
                    )?;
                }
            }
        } else if let Some(symbol) = symbol_and_name.symbol.as_ref() {
            let mut symbol_it = symbol.names()?;
            while let Some((parts, name)) = symbol_it.next_name()? {
                let apis = checker.apis_for_name_iterator(parts);
                if !apis.is_empty() {
                    got_apis = true;
                    (callback)(
                        name.create_name()?,
                        NameSource::Symbol(symbol.clone()),
                        apis,
                    )?;
                }
            }
        }
        if let Some(symbol) = symbol_and_name.symbol.as_ref() {
            if !got_apis {
                // The need to call `to_heap` here is just to get past an annoying variance issue.
                // Fortunately it doesn't seem to affect performance significantly, so probably the
                // optimiser is able to get rid of the allocation.
                if let Some(x) = self.symbol_has_no_apis.get_mut(&symbol.to_heap()) {
                    *x = true;
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum NameSource<'symbol> {
    Symbol(Symbol<'symbol>),
    DebugName(DebugName<'static>),
}

impl NameSource<'_> {
    fn to_owned(&self) -> NameSource<'static> {
        match self {
            NameSource::Symbol(symbol) => NameSource::Symbol(symbol.to_heap()),
            NameSource::DebugName(name) => NameSource::DebugName(name.clone()),
        }
    }
}

impl Display for NameSource<'_> {
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

/// An opaque key for ApiUsages that can be used in a HashMap for deduplication. Notably, doesn't
/// include the target or debug data. The idea is to collect several usages that are identical
/// except for the target, then pick the shortest of them to show to the user. For example if we
/// have targets of `std::path::PathBuf` and `core::ptr::drop_in_place<std::path::PathBuf>` then the
/// second is redundant. Even if the longer target didn't contain the symbol of the shorter target,
/// it's probably unnecessary to show them all.
#[derive(Hash, Eq, PartialEq)]
pub(crate) struct ApiUsageGroupKey {
    pkg_id: PackageId,
    scope: PermissionScope,
    api: ApiName,
    from: SymbolOrDebugName,
    source_location: SourceLocation,
}

impl SingleApiUsage {
    fn group_key(&self) -> ApiUsageGroupKey {
        ApiUsageGroupKey {
            pkg_id: self.pkg_id.clone(),
            scope: self.scope,
            api: self.api.clone(),
            from: self.usage.from.clone(),
            source_location: self.usage.source_location.clone(),
        }
    }
}

/// Additional information that might be useful for debugging. Only available when --debug is
/// passed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum UsageDebugData {
    Relocation(RelocationDebugData),
    Inlined(InlinedDebugData),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RelocationDebugData {
    bin_path: Arc<Path>,
    object_file_path: ObjectFilePath,
    section_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct InlinedDebugData {
    frames: Vec<String>,
    low_pc: Option<u64>,
}

impl InlinedDebugData {
    fn from_offset(
        low_pc: Option<u64>,
        ctx: &addr2line::Context<EndianSlice<LittleEndian>>,
    ) -> Result<InlinedDebugData> {
        let mut frames = Vec::new();
        if let Some(offset) = low_pc {
            let mut frame_iter = ctx.find_frames(offset).skip_all_loads()?;
            while let Some(frame) = frame_iter.next()? {
                if let Some(function) = frame.function.as_ref() {
                    frames.push(Symbol::borrowed(function.name.slice()).to_string());
                }
            }
        }
        Ok(InlinedDebugData { frames, low_pc })
    }
}
