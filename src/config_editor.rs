//! This module is responsible for applying automatic edits to cackle.toml.

use crate::checker::common_prefix::common_to_prefixes;
use crate::config::permissions::PermSel;
use crate::config::ApiName;
use crate::config::ApiPath;
use crate::config::Config;
use crate::config::PackageName;
use crate::config::SandboxKind;
use crate::problem::ApiUsages;
use crate::problem::AvailableApi;
use crate::problem::PossibleExportedApi;
use crate::problem::Problem;
use crate::problem::ProblemList;
use crate::problem::UnusedAllowApi;
use anyhow::anyhow;
use anyhow::Result;
use std::borrow::Borrow;
use std::borrow::Cow;
use std::fmt::Display;
use std::path::Path;
use toml_edit::Array;
use toml_edit::Document;
use toml_edit::Formatted;
use toml_edit::Item;
use toml_edit::Value;

#[derive(Clone)]
pub(crate) struct ConfigEditor {
    document: Document,
}

pub(crate) trait Edit {
    /// Returns a short name for this edit, suitable for display in a menu.
    fn title(&self) -> String;

    fn help(&self) -> Cow<'static, str>;

    /// Applies the edit to the editor.
    fn apply(&self, editor: &mut ConfigEditor, opts: &EditOpts) -> Result<()>;

    /// Returns a list of problems that should replace the current problem after this edit is
    /// applied. This can be used for follow-up actions that need to be performed in order to solve
    /// the original problem.
    fn replacement_problems(&self) -> ProblemList {
        ProblemList::default()
    }

    /// Whether the problem that produced this edit can be resolved if this edit produces no diff.
    /// This should be overriden for any problems that are expected to produce no diff.
    fn resolve_problem_if_edit_is_empty(&self) -> bool {
        true
    }
}

#[derive(Default)]
pub(crate) struct EditOpts {
    /// A comment that the user requested be attached to an edit. Not all edits support adding
    /// comments.
    pub(crate) comment: Option<String>,
}

/// Returns possible fixes for `problem`. The applicability of some fixes depends on the current
/// configuration. Such fixes will only be available if `config` is supplied.
pub(crate) fn fixes_for_problem(problem: &Problem, config: &Config) -> Vec<Box<dyn Edit>> {
    let mut edits: Vec<Box<dyn Edit>> = Vec::new();
    match problem {
        Problem::MissingConfiguration(_) => {
            edits.push(Box::new(CreateRecommendedConfig));
            edits.push(Box::new(CreateCustomConfig));
        }
        Problem::SelectSandbox => {
            for kind in crate::config::SANDBOX_KINDS {
                edits.push(Box::new(SelectSandbox(*kind)));
            }
        }
        Problem::ImportStdApi(api) => {
            edits.push(Box::new(ImportStdApi(api.clone())));
            edits.push(Box::new(InlineStdApi(api.clone())));
            edits.push(Box::new(IgnoreStdApi(api.clone())));
        }
        Problem::AvailableApi(available) => {
            edits.push(Box::new(ImportApi(available.clone())));
            edits.push(Box::new(InlineApi(available.clone())));
            edits.push(Box::new(IgnoreApi(available.clone())));
        }
        Problem::DisallowedApiUsage(usage) => {
            usage.add_allow_api_fixes(&mut edits);
            let _ = usage.add_exclude_fixes(&mut edits, config);
        }
        Problem::IsProcMacro(pkg_id) => {
            edits.push(Box::new(AllowProcMacro {
                perm_sel: PermSel::for_primary(pkg_id.pkg_name()),
            }));
        }
        Problem::BuildScriptFailed(failure) => {
            if failure.output.sandbox_config.kind != SandboxKind::Disabled {
                let perm_sel = PermSel::for_build_script(failure.crate_sel.pkg_name());
                if !failure.output.sandbox_config.allow_network.unwrap_or(false) {
                    edits.push(Box::new(SandboxAllowNetwork {
                        perm_sel: perm_sel.clone(),
                    }));
                }
                edits.push(Box::new(DisableSandbox { perm_sel }));
            }
        }
        Problem::DisallowedBuildInstruction(failure) => {
            edits.append(&mut edits_for_build_instruction(failure));
        }
        Problem::DisallowedUnsafe(failure) => edits.push(Box::new(AllowUnsafe {
            perm_sel: PermSel::for_non_build_output(&failure.crate_sel),
        })),
        Problem::UnusedAllowApi(failure) => edits.push(Box::new(RemoveUnusedAllowApis {
            unused: failure.clone(),
        })),
        Problem::UnusedPackageConfig(crate_name) => edits.push(Box::new(RemoveUnusedPkgConfig {
            perm_sel: crate_name.clone(),
        })),
        Problem::PossibleExportedApi(info) => {
            edits.push(Box::new(ExtendApi {
                api: info.api.clone(),
                api_path: info.api_path(),
            }));
            edits.push(Box::new(NoDetectApi(info.clone())));
        }
        Problem::OffTreeApiUsage(info) => {
            // Ignore errors while adding includes/excludes. Any errors here will likely already
            // have shown up elsewhere and it seems nicer to just degrade to not show those edits.
            let _ = info.usages.add_include_fixes(&mut edits, config);
            let _ = info.usages.add_exclude_fixes(&mut edits, config);
            info.usages.add_allow_api_fixes(&mut edits);
        }
        _ => {}
    }
    edits
}

impl ConfigEditor {
    pub(crate) fn from_file(filename: &Path) -> Result<Self> {
        let toml = std::fs::read_to_string(filename).unwrap_or_default();
        Self::from_toml_string(&toml)
    }

    pub(crate) fn initial() -> Self {
        Self::from_toml_string(r#""#).unwrap()
    }

    pub(crate) fn from_toml_string(toml: &str) -> Result<Self> {
        let document = toml.parse()?;
        Ok(Self { document })
    }

    pub(crate) fn write(&self, filename: &Path) -> Result<()> {
        crate::fs::write_atomic(filename, &self.to_toml())?;
        Ok(())
    }

    pub(crate) fn to_toml(&self) -> String {
        self.document.to_string()
    }

    fn pkg_table(&mut self, perm_sel: &PermSel) -> Result<&mut toml_edit::Table> {
        let path = pkg_path(perm_sel);
        let mut table = self.table(path.clone().take(2))?;
        for part in path.skip(2) {
            table = table
                .entry(part)
                .or_insert_with(create_dotted_table)
                .as_table_mut()
                .ok_or_else(|| anyhow!("[pkg.{perm_sel}] should be a table"))?;
        }
        Ok(table)
    }

    fn opt_pkg_table(&mut self, perm_sel: &PermSel) -> Result<Option<&mut toml_edit::Table>> {
        self.opt_table(pkg_path(perm_sel))
    }

    fn pkg_sandbox_table(&mut self, perm_sel: &PermSel) -> Result<&mut toml_edit::Table> {
        self.table(pkg_path(perm_sel).chain(std::iter::once("sandbox")))
    }

    fn common_table(&mut self) -> Result<&mut toml_edit::Table> {
        self.table(["common"].into_iter())
    }

    fn table<'a>(
        &mut self,
        path: impl Iterator<Item = &'a str> + Clone,
    ) -> Result<&mut toml_edit::Table> {
        let mut table = self.document.as_table_mut();
        let mut it = path.clone();
        while let Some(part) = it.next() {
            let is_last = it.clone().next().is_none();
            table = table
                .entry(part)
                .or_insert_with(if is_last {
                    toml_edit::table
                } else {
                    create_implicit_table
                })
                .as_table_mut()
                .ok_or_else(|| {
                    anyhow!(
                        "[{}] should be a table",
                        path.clone().collect::<Vec<_>>().join(".")
                    )
                })?;
        }
        Ok(table)
    }

    fn opt_table<'a>(
        &mut self,
        path: impl Iterator<Item = &'a str> + Clone,
    ) -> Result<Option<&mut toml_edit::Table>> {
        let mut table = self.document.as_table_mut();
        for part in path.clone() {
            let Some(item) = table.get_mut(part) else {
                return Ok(None);
            };
            table = item.as_table_mut().ok_or_else(|| {
                anyhow!(
                    "[{}] should be a table",
                    path.clone().collect::<Vec<_>>().join(".")
                )
            })?;
        }
        Ok(Some(table))
    }

    pub(crate) fn set_version(&mut self, version: i64) -> Result<()> {
        self.common_table()?
            .entry("version")
            .or_insert_with(|| toml_edit::value(version));
        Ok(())
    }

    pub(crate) fn toggle_std_import(&mut self, api: &str) -> Result<()> {
        let imports = self
            .common_table()?
            .entry("import_std")
            .or_insert_with(create_array)
            .as_array_mut()
            .ok_or_else(|| anyhow!("import_std must be an array"))?;
        if imports.is_empty() {
            imports.set_trailing_comma(true);
        }
        let existing = imports
            .iter()
            .enumerate()
            .find(|(_, item)| item.as_str() == Some(api));
        if let Some((index, _)) = existing {
            imports.remove(index);
        } else {
            imports.push_formatted(create_string(api.to_string(), None));
        }
        Ok(())
    }

    pub(crate) fn set_sandbox_kind(&mut self, sandbox_kind: SandboxKind) -> Result<()> {
        crate::sandbox::verify_kind(sandbox_kind)?;
        let sandbox_kind = match sandbox_kind {
            SandboxKind::Inherit => "Inherit",
            SandboxKind::Disabled => "Disabled",
            SandboxKind::Bubblewrap => "Bubblewrap",
        };
        self.table(["sandbox"].into_iter())?
            .insert("kind", toml_edit::value(sandbox_kind));
        Ok(())
    }
}

impl ApiUsages {
    pub(crate) fn add_include_fixes(
        &self,
        edits: &mut Vec<Box<dyn Edit>>,
        config: &crate::config::Config,
    ) -> Result<()> {
        let api_config = config.get_api_config(&self.api_name)?;
        let common_from_prefixes = crate::checker::common_prefix::common_from_prefixes(self)?;
        for prefix in &common_from_prefixes {
            if api_config
                .exclude
                .iter()
                .any(|p| p.prefix.as_ref() == prefix)
            {
                // Adding an include that exactly matches an exclude doesn't make sense - you'd be
                // better off removing the exclude instead.
                continue;
            }
            if api_config.include.iter().any(|p| {
                p.prefix.as_ref() == prefix
                    || prefix
                        .strip_prefix(p.prefix.as_ref())
                        .map(|remain| remain.starts_with("::"))
                        .unwrap_or(false)
            }) {
                // We already have this prefix or a prefix that would subsume this one, so don't
                // suggest adding it.
                continue;
            }
            edits.push(Box::new(ExtendApi {
                api: self.api_name.clone(),
                api_path: ApiPath::from_str(prefix),
            }));
        }
        Ok(())
    }

    fn add_exclude_fixes(&self, edits: &mut Vec<Box<dyn Edit>>, config: &Config) -> Result<()> {
        let api_config = config.get_api_config(&self.api_name)?;
        let common_to_prefixes = common_to_prefixes(self)?;
        for prefix in common_to_prefixes {
            if api_config
                .include
                .iter()
                .any(|inc| inc.prefix.as_ref() == prefix)
            {
                // Adding an exclude that exactly matches an include doesn't make sense - you'd be
                // better off removing the include instead.
                continue;
            }
            // Only propose excluding a path if it's an extension of an existing inclusion. e.g. if
            // we have an include of `std::path` then we can exclude `std::path::Path`, but not
            // `std::collections::HashMap`.
            if api_config.include.iter().any(|inc| {
                prefix
                    .strip_prefix(inc.prefix.as_ref())
                    .map(|remaining| remaining.starts_with("::"))
                    .unwrap_or(false)
            }) {
                edits.push(Box::new(ExcludeFromApi {
                    api: self.api_name.clone(),
                    api_path: ApiPath::from_str(&prefix),
                }));
            }
        }
        Ok(())
    }

    fn add_allow_api_fixes(&self, edits: &mut Vec<Box<dyn Edit>>) {
        edits.push(Box::new(AllowApiUsage {
            usage: self.clone(),
        }));
        let mut scope = self.scope;
        while let Some(parent_scope) = scope.parent_scope() {
            edits.push(Box::new(AllowApiUsage {
                usage: ApiUsages {
                    scope: parent_scope,
                    ..self.clone()
                },
            }));
            scope = parent_scope;
        }
    }
}

fn pkg_path(perm_sel: &PermSel) -> impl Iterator<Item = &str> + Clone {
    std::iter::once("pkg")
        .chain(std::iter::once(perm_sel.package_name.as_ref()))
        .chain(
            perm_sel
                .scope
                .config_selector()
                .into_iter()
                .flat_map(|s| s.split('.')),
        )
}

fn edits_for_build_instruction(
    failure: &crate::problem::DisallowedBuildInstruction,
) -> Vec<Box<dyn Edit>> {
    let mut out: Vec<Box<dyn Edit>> = Vec::new();
    let mut instruction = failure.instruction.as_str();
    let mut suffix = "";
    loop {
        out.push(Box::new(AllowBuildInstruction {
            perm_sel: PermSel::for_build_script(failure.pkg_id.name_str()),
            instruction: format!("{instruction}{suffix}"),
        }));
        suffix = "*";
        let mut separators = "=-:";
        let mut last_separator = None;
        instruction = &instruction[..instruction.len() - 1];
        for (pos, ch) in instruction.char_indices() {
            if separators.contains(ch) {
                last_separator = Some(pos);
                // After =, only permit = as a separator. i.e. ':' and '-' are only separators up to
                // the first equals.
                if ch == '=' {
                    separators = "="
                }
            }
        }
        if let Some(last_separator) = last_separator {
            instruction = &instruction[..last_separator + 1];
        } else {
            break;
        }
    }
    out
}

fn create_array() -> Item {
    let mut array = Array::new();
    array.set_trailing_comma(true);
    array.set_trailing("\n");
    Item::Value(Value::Array(array))
}

fn create_implicit_table() -> Item {
    let mut table = toml_edit::Table::new();
    table.set_implicit(true);
    Item::Table(table)
}

fn create_dotted_table() -> Item {
    let mut table = toml_edit::Table::new();
    table.set_dotted(true);
    Item::Table(table)
}

struct CreateCustomConfig;

impl Edit for CreateCustomConfig {
    fn title(&self) -> String {
        "Create custom initial config".to_owned()
    }

    fn help(&self) -> Cow<'static, str> {
        "Writes a cackle.toml into your workspace / crate root. This will initially only set the \
        configuration version. Subsequent action items will prompt you to select a sandbox kind, \
        select what APIs you care about etc."
            .into()
    }

    fn apply(&self, editor: &mut ConfigEditor, _opts: &EditOpts) -> Result<()> {
        editor.set_version(crate::config::MAX_VERSION)
    }

    fn replacement_problems(&self) -> ProblemList {
        let mut problems = ProblemList::default();
        problems.push(Problem::SelectSandbox);
        for api in crate::config::built_in::get_built_ins().keys() {
            problems.push(Problem::ImportStdApi(api.clone()));
        }
        problems
    }
}

struct CreateRecommendedConfig;

impl Edit for CreateRecommendedConfig {
    fn title(&self) -> String {
        "Create recommended initial config".to_owned()
    }

    fn help(&self) -> Cow<'static, str> {
        "Writes a cackle.toml into your workspace / crate root with imports for the fs, net and \
        process APIs."
            .into()
    }

    fn apply(&self, editor: &mut ConfigEditor, _opts: &EditOpts) -> Result<()> {
        editor.set_version(crate::config::MAX_VERSION)?;
        editor.toggle_std_import("fs")?;
        editor.toggle_std_import("net")?;
        editor.toggle_std_import("process")?;
        Ok(())
    }

    fn replacement_problems(&self) -> ProblemList {
        // We leave selecting the sandbox to a separate step, since it does validation that the
        // sandbox is installed.
        Problem::SelectSandbox.into()
    }
}

struct SelectSandbox(SandboxKind);

impl Edit for SelectSandbox {
    fn title(&self) -> String {
        format!("{:?}", self.0)
    }

    fn help(&self) -> Cow<'static, str> {
        "Select what kind of sandbox you'd like to use. At the moment the sandbox is only used \
         for running build scripts (build.rs) and tests. Hopefully eventually we'll also run \
         proc-macros in the sandbox. To use Bubblewrap, it must be installed. On Debian-based \
         systems you can `sudo apt install bubblewrap`"
            .into()
    }

    fn apply(&self, editor: &mut ConfigEditor, _opts: &EditOpts) -> Result<()> {
        editor.set_sandbox_kind(self.0)
    }
}

struct ImportStdApi(ApiName);

impl Edit for ImportStdApi {
    fn title(&self) -> String {
        format!("Import std API `{}`", self.0)
    }

    fn help(&self) -> Cow<'static, str> {
        "This imports an std API that's built into Cackle. Subsequent versions of Cackle may \
         add/remove paths from this API if it turns out that there were inaccuracies."
            .into()
    }

    fn apply(&self, editor: &mut ConfigEditor, _opts: &EditOpts) -> Result<()> {
        editor.toggle_std_import(self.0.name.borrow())
    }
}

struct ImportApi(AvailableApi);

impl Edit for ImportApi {
    fn title(&self) -> String {
        format!(
            "Import API `{}` from package `{}`",
            self.0.api,
            PackageName::from(&self.0.pkg_id)
        )
    }

    fn help(&self) -> Cow<'static, str> {
        "Imports an API definition that was provided by a third-party crate. Future versions of \
         that crate may adjust these API definitions, hopefully to make them more accurate or \
         complete."
            .into()
    }

    fn apply(&self, editor: &mut ConfigEditor, opts: &EditOpts) -> Result<()> {
        let table = editor.pkg_table(&PermSel::for_primary(self.0.pkg_id.name_str()))?;
        add_to_array(
            table,
            "import",
            &[&self.0.api.name],
            opts.comment.as_deref(),
        )
    }
}

struct InlineStdApi(ApiName);

impl Edit for InlineStdApi {
    fn title(&self) -> String {
        format!("Inline std API `{}`", self.0)
    }

    fn help(&self) -> Cow<'static, str> {
        "This copies the built-in API definition into your cackle.toml. Changes to the API \
         definition in future versions of cackle will not affect your configuration. Selecting \
         this option gives you the ability to adjust the API definitions from those that are \
         built-in."
            .into()
    }

    fn apply(&self, editor: &mut ConfigEditor, _opts: &EditOpts) -> Result<()> {
        let table = editor.table(["api", self.0.name.as_ref()].into_iter())?;
        let built_ins = crate::config::built_in::get_built_ins();
        let api_config = built_ins
            .get(&self.0)
            .ok_or_else(|| anyhow!("Attempted to inline unknown API `{}`", self.0))?;
        add_to_array(table, "include", &api_config.include, None)?;
        add_to_array(table, "exclude", &api_config.exclude, None)?;
        Ok(())
    }
}

struct InlineApi(AvailableApi);

impl Edit for InlineApi {
    fn title(&self) -> String {
        format!(
            "Inline API `{}` from package `{}`",
            self.0.api, self.0.pkg_id
        )
    }

    fn help(&self) -> Cow<'static, str> {
        "Inlines an API definition from a third-party crate. This lets you adjust this API \
         definition. It does however mean that any changes made to the API definition by the \
         third-party crate will not be used, so for example if a crate, `foo` exported network \
         APIs under `foo::net` and later started also exporting them under `foo::network`, then \
         you might miss these. So care should be taken if selecting this option."
            .into()
    }

    fn apply(&self, editor: &mut ConfigEditor, _opts: &EditOpts) -> Result<()> {
        let table = editor.table(["api", self.0.api.name.as_ref()].into_iter())?;
        add_to_array(table, "include", &self.0.config.include, None)?;
        add_to_array(table, "exclude", &self.0.config.exclude, None)?;
        // We also need to ignore it, otherwise we'll keep warning about it.
        IgnoreApi(self.0.clone()).apply(editor, &Default::default())
    }
}

fn add_to_array<S: AsRef<str>>(
    table: &mut toml_edit::Table,
    array_name: &str,
    values: &[S],
    comment: Option<&str>,
) -> Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    let array = get_or_create_array(table, array_name)?;
    for v in values {
        let value = v.as_ref().to_owned();
        // Insert our new value before the first element greater than our new value. This will
        // maintain alphabetical order if the list is currently sorted.
        let index = array
            .iter()
            .enumerate()
            .find(|(_, existing)| {
                existing
                    .as_str()
                    .map(|e| e >= value.as_str())
                    .unwrap_or(false)
            })
            .map(|(index, _)| index)
            .unwrap_or_else(|| array.len());
        if array
            .get(index)
            .and_then(Value::as_str)
            .map(|existing| existing == value)
            .unwrap_or(false)
        {
            // Value is already present in the array.
            continue;
        }
        array.insert_formatted(index, create_string(value, comment));
    }
    Ok(())
}

struct IgnoreStdApi(ApiName);

impl Edit for IgnoreStdApi {
    fn title(&self) -> String {
        format!("Ignore std API `{}`", self.0)
    }

    fn help(&self) -> Cow<'static, str> {
        "Don't import or inline this API definition. Select this if you don't care if crates use \
        this category of API."
            .into()
    }

    fn apply(&self, _editor: &mut ConfigEditor, _opts: &EditOpts) -> Result<()> {
        Ok(())
    }

    fn resolve_problem_if_edit_is_empty(&self) -> bool {
        false
    }
}

struct IgnoreApi(AvailableApi);

impl Edit for IgnoreApi {
    fn title(&self) -> String {
        format!(
            "Ignore API `{}` provided by package `{}`",
            self.0.api, self.0.pkg_id
        )
    }

    fn help(&self) -> Cow<'static, str> {
        "Don't import or inline this API definition. Select this if you don't care if crates use \
        this category of API."
            .into()
    }

    fn apply(&self, editor: &mut ConfigEditor, _opts: &EditOpts) -> Result<()> {
        // Make sure the `import` table exists, otherwise we'll continue to warn about unused
        // imports.
        let table = editor.pkg_table(&PermSel::for_primary(self.0.pkg_id.pkg_name()))?;
        get_or_create_array(table, "import")?;
        Ok(())
    }

    fn resolve_problem_if_edit_is_empty(&self) -> bool {
        false
    }
}

struct ExtendApi {
    api: ApiName,
    api_path: ApiPath,
}

impl Edit for ExtendApi {
    fn title(&self) -> String {
        format!("Include `{}` in API `{}`", self.api_path, self.api)
    }

    fn help(&self) -> Cow<'static, str> {
        format!(
            "Classify usages of `{}` as the API `{}`",
            self.api_path, self.api
        )
        .into()
    }

    fn apply(&self, editor: &mut ConfigEditor, opts: &EditOpts) -> Result<()> {
        let table = editor.table(["api", self.api.name.as_ref()].into_iter())?;
        add_to_array(table, "include", &[&self.api_path], opts.comment.as_deref())?;
        Ok(())
    }
}

struct ExcludeFromApi {
    api: ApiName,
    api_path: ApiPath,
}

impl Edit for ExcludeFromApi {
    fn title(&self) -> String {
        format!("Exclude `{}` from API `{}`", self.api_path, self.api)
    }

    fn help(&self) -> Cow<'static, str> {
        format!(
            "Don't classify paths starting with `{}` as the API `{}`",
            self.api_path, self.api
        )
        .into()
    }

    fn apply(&self, editor: &mut ConfigEditor, opts: &EditOpts) -> Result<()> {
        let table = editor.table(["api", self.api.name.as_ref()].into_iter())?;
        add_to_array(table, "exclude", &[&self.api_path], opts.comment.as_deref())?;
        Ok(())
    }
}

struct NoDetectApi(PossibleExportedApi);

impl Edit for NoDetectApi {
    fn title(&self) -> String {
        format!(
            "Don't detect `{}` in `{}`",
            self.0.api,
            self.0.pkg_id.name_str(),
        )
    }

    fn help(&self) -> Cow<'static, str> {
        format!(
            "Ignore this possible exported API. Select this if you've looked at `{}` and \
             determined that it doesn't export the API `{}`",
            self.0.pkg_id.name_str(),
            self.0.api,
        )
        .into()
    }

    fn apply(&self, editor: &mut ConfigEditor, opts: &EditOpts) -> Result<()> {
        let table = editor.table(["api", self.0.api.name.as_ref()].into_iter())?;
        add_to_array(
            table,
            "no_auto_detect",
            &[self.0.pkg_id.name_str()],
            opts.comment.as_deref(),
        )?;
        Ok(())
    }
}

struct AllowApiUsage {
    usage: ApiUsages,
}

impl Edit for AllowApiUsage {
    fn title(&self) -> String {
        let pkg = &self.usage.pkg_id;
        let api = &self.usage.api_name;
        match self.usage.scope {
            crate::config::permissions::PermissionScope::All => {
                format!("Allow `{pkg}` to use `{api}` API")
            }
            crate::config::permissions::PermissionScope::Build => {
                format!("Allow `{pkg}` to use `{api}` API when building its own build script")
            }
            crate::config::permissions::PermissionScope::Test => {
                format!("Allow `{pkg}` to use `{api}` API when building its own tests")
            }
            crate::config::permissions::PermissionScope::FromBuild => {
                format!("Allow `{pkg}` to use `{api}` API when building build scripts")
            }
            crate::config::permissions::PermissionScope::FromTest => {
                format!("Allow `{pkg}` to use `{api}` API when building tests")
            }
        }
    }

    fn help(&self) -> Cow<'static, str> {
        let pkg = &self.usage.pkg_id;
        let api = &self.usage.api_name;
        match self.usage.scope {
            crate::config::permissions::PermissionScope::All => {
                format!("Allow `{pkg}` to use `{api}` API in any binary")
            }
            crate::config::permissions::PermissionScope::Build => {
                format!("Allow `{pkg}` to use `{api}` API, but only in its own build script")
            }
            crate::config::permissions::PermissionScope::Test => {
                format!("Allow `{pkg}` to use `{api}` API, but only in its own tests")
            }
            crate::config::permissions::PermissionScope::FromBuild => {
                format!("Allow `{pkg}` to use `{api}` API, but only from build scripts")
            }
            crate::config::permissions::PermissionScope::FromTest => {
                format!("Allow `{pkg}` to use `{api}` API, but only from tests")
            }
        }
        .into()
    }

    fn apply(&self, editor: &mut ConfigEditor, opts: &EditOpts) -> Result<()> {
        let table = editor.pkg_table(&self.usage.perm_sel())?;
        add_to_array(
            table,
            "allow_apis",
            &[&self.usage.api_name],
            opts.comment.as_deref(),
        )?;
        // Remove the API from any selectors that inherit from the one that we just added to, since
        // they're now redundant.
        for perm_sel in self.usage.perm_sel().descendants() {
            RemoveUnusedAllowApis {
                unused: UnusedAllowApi {
                    perm_sel,
                    apis: vec![self.usage.api_name.clone()],
                },
            }
            .apply(editor, opts)?;
        }
        Ok(())
    }
}

struct RemoveUnusedAllowApis {
    unused: UnusedAllowApi,
}

impl Edit for RemoveUnusedAllowApis {
    fn title(&self) -> String {
        "Remove unused allowed APIs".to_owned()
    }

    fn help(&self) -> Cow<'static, str> {
        "Remove these APIs from the list of APIs that this package is allowed to used.".into()
    }

    fn apply(&self, editor: &mut ConfigEditor, _opts: &EditOpts) -> Result<()> {
        let Some(table) = editor.opt_pkg_table(&self.unused.perm_sel)? else {
            return Ok(());
        };
        let Some(allow_apis) = get_array(table, "allow_apis")? else {
            return Ok(());
        };
        for api in &self.unused.apis {
            let index_and_entry = allow_apis
                .iter()
                .enumerate()
                .find(|(_, allowed)| allowed.as_str() == Some(api.to_string().as_str()));
            if let Some((index, _)) = index_and_entry {
                allow_apis.remove(index);
            }
        }
        if allow_apis.is_empty() {
            table.remove("allow_apis");
        }
        Ok(())
    }
}

struct RemoveUnusedPkgConfig {
    perm_sel: PermSel,
}

impl Edit for RemoveUnusedPkgConfig {
    fn title(&self) -> String {
        "Remove unused crate configuration".to_owned()
    }

    fn help(&self) -> Cow<'static, str> {
        "Remove the configuration for this package.".into()
    }

    fn apply(&self, editor: &mut ConfigEditor, _opts: &EditOpts) -> Result<()> {
        let mut path: Vec<_> = pkg_path(&self.perm_sel).collect();
        let last_part = path.pop().unwrap();
        if let Some(parent_table) = editor.opt_table(path.into_iter())? {
            parent_table.remove(last_part);
        }
        Ok(())
    }
}

fn get_array<'table>(
    table: &'table mut toml_edit::Table,
    array_name: &str,
) -> Result<Option<&'table mut Array>> {
    let Some(item) = table.get_mut(array_name) else {
        return Ok(None);
    };
    let array = item
        .as_array_mut()
        .ok_or_else(|| anyhow!("{array_name} should be an array"))?;
    Ok(Some(array))
}

fn get_or_create_array<'table>(
    table: &'table mut toml_edit::Table,
    array_name: &str,
) -> Result<&'table mut Array> {
    let array = table
        .entry(array_name)
        .or_insert_with(create_array)
        .as_array_mut()
        .ok_or_else(|| anyhow!("{array_name} should be an array"))?;
    if array.is_empty() {
        array.set_trailing_comma(true);
    }
    Ok(array)
}

fn create_string(value: String, comment: Option<&str>) -> Value {
    let string = Value::String(Formatted::new(value));
    if let Some(comment) = comment {
        string.decorated(format!("\n    # {comment}\n    "), "")
    } else {
        string.decorated("\n    ", "")
    }
}

struct AllowProcMacro {
    perm_sel: PermSel,
}

impl Edit for AllowProcMacro {
    fn title(&self) -> String {
        format!("Allow proc macro `{}`", self.perm_sel)
    }

    fn help(&self) -> Cow<'static, str> {
        "Allow this crate to be a proc macro. Proc macros can generate arbitrary code. They're \
         also not currently run in a sandbox."
            .into()
    }

    fn apply(&self, editor: &mut ConfigEditor, opts: &EditOpts) -> Result<()> {
        let table = editor.pkg_table(&self.perm_sel)?;
        set_table_value(table, "allow_proc_macro", toml_edit::value(true), opts);
        Ok(())
    }
}

struct AllowBuildInstruction {
    perm_sel: PermSel,
    instruction: String,
}

impl Edit for AllowBuildInstruction {
    fn title(&self) -> String {
        format!(
            "Allow build script for `{}` to emit instruction `{}`",
            self.perm_sel, self.instruction
        )
    }

    fn help(&self) -> Cow<'static, str> {
        "Allow this crate's build.rs to emit build instructions that match the specified pattern. \
         Some build instructions can be used to add arguments to the linker, which can then be \
         used to do just about anything."
            .into()
    }

    fn apply(&self, editor: &mut ConfigEditor, opts: &EditOpts) -> Result<()> {
        let table = editor.pkg_table(&self.perm_sel)?;
        add_to_array(
            table,
            "allow_build_instructions",
            &[&self.instruction],
            opts.comment.as_deref(),
        )
    }
}

struct DisableSandbox {
    perm_sel: PermSel,
}

impl Edit for DisableSandbox {
    fn title(&self) -> String {
        format!("Disable sandbox for `{}`", self.perm_sel)
    }

    fn help(&self) -> Cow<'static, str> {
        "Don't run this crate's build script (build.rs) in a sandbox. You might select this \
         option if the build script is doing something weird like writing to the source \
         directory, but you've checked it over and you trust it."
            .into()
    }

    fn apply(&self, editor: &mut ConfigEditor, opts: &EditOpts) -> Result<()> {
        let table = editor.pkg_sandbox_table(&self.perm_sel)?;
        set_table_value(table, "kind", toml_edit::value("Disabled"), opts);
        Ok(())
    }
}

struct AllowUnsafe {
    perm_sel: PermSel,
}

impl Edit for AllowUnsafe {
    fn title(&self) -> String {
        format!("Allow package `{}` to use unsafe code", self.perm_sel)
    }

    fn help(&self) -> Cow<'static, str> {
        "Allow this crate to use unsafe code. With unsafe code, this crate could do just about \
         anything, so this is like a bit like a wildcard permission. Crates that use unsafe \
         sometimes export APIs that you might want to restrict - e.g. network or filesystem APIs. \
         so you should have a think about if this crate falls into that category and if it does, \
         add some API definitions for it."
            .into()
    }

    fn apply(&self, editor: &mut ConfigEditor, opts: &EditOpts) -> Result<()> {
        let table = editor.pkg_table(&self.perm_sel)?;
        set_table_value(table, "allow_unsafe", toml_edit::value(true), opts);
        Ok(())
    }
}

struct SandboxAllowNetwork {
    perm_sel: PermSel,
}

impl Edit for SandboxAllowNetwork {
    fn title(&self) -> String {
        format!("Permit network from sandbox for `{}`", self.perm_sel)
    }

    fn help(&self) -> Cow<'static, str> {
        "Allow this crate's build script (build.rs) to access the network. This might be necessary \
         if the build script is downloading stuff from the Internet."
            .into()
    }

    fn apply(&self, editor: &mut ConfigEditor, opts: &EditOpts) -> Result<()> {
        let table = editor.pkg_sandbox_table(&self.perm_sel)?;
        set_table_value(table, "allow_network", toml_edit::value(true), opts);
        Ok(())
    }
}

impl Display for dyn Edit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.title())
    }
}

fn set_table_value(
    table: &mut toml_edit::Table,
    key: &str,
    item: toml_edit::Item,
    opts: &EditOpts,
) {
    table[key] = item;
    if let Some(comment) = &opts.comment {
        if let Some(decor) = table.key_decor_mut(key) {
            *decor = toml_edit::Decor::new(format!("# {comment}\n"), " ");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ConfigEditor;
    use super::Edit;
    use super::InlineStdApi;
    use crate::config::permissions::PermSel;
    use crate::config::permissions::PermissionScope;
    use crate::config::ApiName;
    use crate::config::Config;
    use crate::config::SandboxConfig;
    use crate::config_editor::fixes_for_problem;
    use crate::crate_index::testing::pkg_id;
    use crate::crate_index::CrateSel;
    use crate::crate_index::PackageId;
    use crate::location::SourceLocation;
    use crate::problem::ApiUsages;
    use crate::problem::DisallowedBuildInstruction;
    use crate::problem::Problem;
    use crate::proxy::rpc::BinExecutionOutput;
    use indoc::indoc;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn disallowed_api(pkg_id: PackageId, scope: PermissionScope, api: &'static str) -> Problem {
        Problem::DisallowedApiUsage(ApiUsages {
            pkg_id: pkg_id.clone(),
            scope,
            api_name: ApiName::from(api),
            usages: Vec::new(),
        })
    }

    #[track_caller]
    fn check(initial_config: &str, problem: &Problem, fix_index: usize, expected: &str) {
        let config = crate::config::testing::parse(initial_config).unwrap();
        let mut editor = ConfigEditor::from_toml_string(initial_config).unwrap();
        let edit = &fixes_for_problem(problem, &config)[fix_index];
        edit.apply(&mut editor, &Default::default()).unwrap();
        let updated_toml = editor.to_toml();
        assert_eq!(updated_toml, expected);

        // Apply the edit a second time and make sure that the result doesn't change.
        let mut editor = ConfigEditor::from_toml_string(&updated_toml).unwrap();
        edit.apply(&mut editor, &Default::default()).unwrap();
        assert_eq!(editor.to_toml(), expected);
    }

    #[test]
    fn fix_missing_api_no_existing_config() {
        check(
            "",
            &disallowed_api(pkg_id("crab1"), PermissionScope::All, "fs"),
            0,
            indoc! {r#"
                [pkg.crab1]
                allow_apis = [
                    "fs",
                ]
            "#,
            },
        );
    }

    #[test]
    fn fix_missing_api_build_script() {
        check(
            "",
            &disallowed_api(pkg_id("crab1"), PermissionScope::Build, "fs"),
            0,
            indoc! {r#"
                [pkg.crab1]
                build.allow_apis = [
                    "fs",
                ]
            "#,
            },
        );
    }

    #[test]
    fn allow_api_existing_sub_selector() {
        check(
            indoc! {r#"
                import_std = ["fs", "net", "process"]
                [pkg.crab1]
                allow_apis = [
                    "net",
                ]
                test.allow_apis = [
                    "fs",
                    "process",
                ]
            "#},
            &disallowed_api(pkg_id("crab1"), PermissionScope::All, "fs"),
            0,
            indoc! {r#"
                import_std = ["fs", "net", "process"]
                [pkg.crab1]
                allow_apis = [
                    "fs",
                    "net",
                ]
                test.allow_apis = [
                    "process",
                ]
            "#},
        );
    }

    #[test]
    fn fix_disallowed_build_instruction() {
        let problem = Problem::DisallowedBuildInstruction(DisallowedBuildInstruction {
            pkg_id: pkg_id("crab1"),
            instruction: "cargo:rustc-env=SOME_VAR=/home/some-path".to_owned(),
        });
        check(
            "",
            &problem,
            1,
            indoc! {r#"
                [pkg.crab1]
                build.allow_build_instructions = [
                    "cargo:rustc-env=SOME_VAR=*",
                ]
            "#,
            },
        );
    }

    #[test]
    fn fix_missing_api_existing_config() {
        check(
            indoc! {r#"
                [api.env]
                [api.net]
                [api.fs]
                [pkg.crab1]
                allow_apis = [
                    "env",
                    "net",
                ]
            "#},
            &disallowed_api(pkg_id("crab1"), PermissionScope::All, "fs"),
            0,
            indoc! {r#"
                [api.env]
                [api.net]
                [api.fs]
                [pkg.crab1]
                allow_apis = [
                    "env",
                    "fs",
                    "net",
                ]
            "#},
        );
    }

    #[test]
    fn fix_missing_api_existing_empty_config() {
        check(
            indoc! {r#"
                [pkg.crab1]
                allow_apis = [
                ]
            "#},
            &disallowed_api(pkg_id("crab1"), PermissionScope::All, "net"),
            0,
            indoc! {r#"
                [pkg.crab1]
                allow_apis = [
                    "net",
                ]
            "#},
        );
    }

    #[test]
    fn fix_allow_proc_macro() {
        check(
            "",
            &Problem::IsProcMacro(pkg_id("crab1")),
            0,
            indoc! {r#"
                [pkg.crab1]
                allow_proc_macro = true
            "#,
            },
        );
    }

    #[test]
    fn fix_allow_unsafe() {
        check(
            "",
            &Problem::DisallowedUnsafe(crate::proxy::rpc::UnsafeUsage {
                crate_sel: CrateSel::primary(pkg_id("crab1")),
                locations: vec![SourceLocation::new(Path::new("main.rs"), 10, None)],
            }),
            0,
            indoc! {r#"
                [pkg.crab1]
                allow_unsafe = true
            "#,
            },
        );
    }

    #[test]
    fn build_script_failed() {
        let crate_sel = CrateSel::build_script(pkg_id("crab1"));
        let failure = Problem::BuildScriptFailed(crate::problem::BinExecutionFailed {
            output: BinExecutionOutput {
                exit_code: 1,
                stdout: Vec::new(),
                stderr: Vec::new(),
                crate_sel: crate_sel.clone(),
                sandbox_config: SandboxConfig {
                    kind: crate::config::SandboxKind::Bubblewrap,
                    extra_args: vec![],
                    allow_network: None,
                    bind_writable: vec![],
                    make_writable: vec![],
                },
                build_script: PathBuf::new(),
                sandbox_config_display: None,
            },
            crate_sel,
        });
        check(
            "",
            &failure,
            0,
            indoc! {r#"
                [pkg.crab1.build.sandbox]
                allow_network = true
            "#,
            },
        );
        check(
            "",
            &failure,
            1,
            indoc! {r#"
                [pkg.crab1.build.sandbox]
                kind = "Disabled"
            "#,
            },
        );
    }

    #[test]
    fn unused_allow_api() {
        let failure = Problem::UnusedAllowApi(crate::problem::UnusedAllowApi {
            perm_sel: PermSel::for_build_script("crab1"),
            apis: vec![ApiName::new("fs"), ApiName::new("net")],
        });
        check(
            indoc! {r#"
                [api.fs]
                [api.env]
                [api.net]
                [pkg.crab1.build]
                allow_apis = [
                    "fs",
                    "env",
                    "net",
                ]
            "#},
            &failure,
            0,
            indoc! {r#"
                [api.fs]
                [api.env]
                [api.net]
                [pkg.crab1.build]
                allow_apis = [
                    "env",
                ]
            "#,
            },
        );
    }

    #[test]
    fn unused_allow_api_empty() {
        let failure = Problem::UnusedAllowApi(crate::problem::UnusedAllowApi {
            perm_sel: PermSel::for_build_script("crab1"),
            apis: vec![ApiName::new("fs"), ApiName::new("net")],
        });
        check(
            indoc! {r#"
                [api.fs]
                [api.net]
                [pkg.crab1.build]
                allow_unsafe = true
                allow_apis = [
                    "fs",
                    "net",
                ]
            "#},
            &failure,
            0,
            indoc! {r#"
                [api.fs]
                [api.net]
                [pkg.crab1.build]
                allow_unsafe = true
            "#,
            },
        );
    }

    #[test]
    fn unused_allow_api_already_deleted() {
        let failure = Problem::UnusedAllowApi(crate::problem::UnusedAllowApi {
            perm_sel: PermSel::for_primary("crab1"),
            apis: vec![ApiName::new("fs")],
        });
        // If another edit (e.g. removal of an unused pkg config) removed our table, make sure we
        // don't recreate it.
        check("", &failure, 0, "");
    }

    #[test]
    fn unused_pkg_config_build_script() {
        let failure = Problem::UnusedPackageConfig(PermSel::for_build_script("crab1"));
        check(
            indoc! {r#"
                [pkg.crab1]
                allow_unsafe = true

                [pkg.crab1.build]
                allow_unsafe = true
            "#},
            &failure,
            0,
            indoc! {r#"
                [pkg.crab1]
                allow_unsafe = true
            "#,
            },
        );

        check(
            indoc! {r#"
                [pkg.crab1.build]
                allow_unsafe = true
            "#,
            },
            &failure,
            0,
            "",
        );
    }

    #[test]
    fn unused_pkg_config() {
        let failure = Problem::UnusedPackageConfig(PermSel::for_primary("crab2"));
        check(
            indoc! {r#"
                [api.fs]
                [pkg.crab1]
                allow_unsafe = true

                [pkg.crab2]
                allow_apis = ["fs"]
            "#},
            &failure,
            0,
            indoc! {r#"
                [api.fs]
                [pkg.crab1]
                allow_unsafe = true
            "#,
            },
        );
    }

    fn apply_edit_and_parse(toml: &str, edit: &InlineStdApi) -> Arc<Config> {
        let mut editor = ConfigEditor::from_toml_string(toml).unwrap();
        edit.apply(&mut editor, &Default::default()).unwrap();
        crate::config::testing::parse(&editor.to_toml()).unwrap()
    }

    #[test]
    fn inline_std_api() {
        let fs_api = ApiName::new("fs");
        let edit = &InlineStdApi(fs_api.clone());
        let config = apply_edit_and_parse("", edit);
        let built_ins = crate::config::built_in::get_built_ins();
        assert_eq!(built_ins.get(&fs_api), config.raw.apis.get(&fs_api));
    }
}
