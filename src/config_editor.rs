//! This module is responsible for applying automatic edits to cackle.toml.

use crate::config::SandboxKind;
use crate::problem::DisallowedApiUsage;
use crate::problem::Problem;
use crate::problem::UnusedAllowApi;
use anyhow::anyhow;
use anyhow::Result;
use std::path::Path;
use toml_edit::Array;
use toml_edit::Document;
use toml_edit::Formatted;
use toml_edit::Item;
use toml_edit::Value;

pub(crate) struct ConfigEditor {
    document: Document,
}

pub(crate) trait Edit {
    /// Returns a short name for this edit, suitable for display in a menu.
    fn title(&self) -> String;

    /// Applies the edit to the editor.
    fn apply(&self, editor: &mut ConfigEditor) -> Result<()>;
}

/// Returns possible fixes for `problem`.
pub(crate) fn fixes_for_problem(problem: &Problem) -> Vec<Box<dyn Edit>> {
    let mut edits: Vec<Box<dyn Edit>> = Vec::new();
    match problem {
        Problem::DisallowedApiUsage(usage) => {
            edits.push(Box::new(AllowApiUsage {
                usage: usage.clone(),
            }));
        }
        Problem::IsProcMacro(pkg_name) => {
            edits.push(Box::new(AllowProcMacro {
                pkg_name: pkg_name.clone(),
            }));
        }
        Problem::BuildScriptFailed(failure) => {
            if failure.output.sandbox_config.kind != SandboxKind::Disabled {
                edits.push(Box::new(DisableSandbox {
                    pkg_name: failure.output.package_name.clone(),
                }));
                if !failure.output.sandbox_config.allow_network.unwrap_or(false) {
                    edits.push(Box::new(SandboxAllowNetwork {
                        pkg_name: failure.output.package_name.clone(),
                    }));
                }
            }
        }
        Problem::DisallowedBuildInstruction(failure) => {
            edits.append(&mut edits_for_build_instruction(failure));
        }
        Problem::DisallowedUnsafe(failure) => edits.push(Box::new(AllowUnsafe {
            pkg_name: failure.crate_name.to_owned(),
        })),
        Problem::UnusedAllowApi(failure) => edits.push(Box::new(RemoveUnusedAllowApis {
            unused: failure.clone(),
        })),
        _ => {}
    }
    edits
}

impl ConfigEditor {
    pub(crate) fn from_file(filename: &Path) -> Result<Self> {
        let toml = std::fs::read_to_string(filename)?;
        Self::from_toml_string(&toml)
    }

    pub(crate) fn initial() -> Self {
        Self::from_toml_string(r#""#).unwrap()
    }

    fn from_toml_string(toml: &str) -> Result<Self> {
        let document = toml.parse()?;
        Ok(Self { document })
    }

    pub(crate) fn write(&self, filename: &Path) -> Result<()> {
        std::fs::write(filename, self.to_toml())?;
        Ok(())
    }

    pub(crate) fn to_toml(&self) -> String {
        self.document.to_string()
    }

    fn table(&mut self, pkg_name: &str) -> Result<&mut toml_edit::Table> {
        let mut pkg = self
            .document
            .as_table_mut()
            .entry("pkg")
            .or_insert_with(create_implicit_table)
            .as_table_mut()
            .ok_or_else(|| anyhow!("[pkg] should be a table"))?;
        let mut parts = pkg_name.split('.').peekable();
        while let Some(part) = parts.next() {
            let is_last = parts.peek().is_none();
            pkg = pkg
                .entry(part)
                .or_insert_with(if is_last {
                    toml_edit::table
                } else {
                    create_implicit_table
                })
                .as_table_mut()
                .ok_or_else(|| anyhow!("[pkg.{pkg_name}] should be a table"))?;
        }
        Ok(pkg)
    }

    pub(crate) fn set_version(&mut self, version: i64) {
        self.document
            .as_table_mut()
            .entry("version")
            .or_insert_with(|| toml_edit::value(version));
    }

    pub(crate) fn std_imports(&self) -> Option<impl Iterator<Item = &str>> {
        Some(
            self.document
                .as_table()
                .get("import_std")?
                .as_array()?
                .iter()
                .filter_map(|imp| imp.as_str()),
        )
    }

    pub(crate) fn toggle_std_import(&mut self, api: &str) -> Result<()> {
        let imports = self
            .document
            .as_table_mut()
            .entry("import_std")
            .or_insert_with(create_array)
            .as_array_mut()
            .ok_or_else(|| anyhow!("import_std must be an array"))?;
        let existing = imports
            .iter()
            .enumerate()
            .find(|(_, item)| item.as_str() == Some(api));
        if let Some((index, _)) = existing {
            imports.remove(index);
        } else {
            imports.push_formatted(create_string(api.to_string()));
        }
        Ok(())
    }

    pub(crate) fn set_sandbox_kind(&mut self, sandbox_kind: SandboxKind) -> Result<()> {
        let sandbox_kind = match sandbox_kind {
            SandboxKind::Inherit => "Inherit",
            SandboxKind::Disabled => "Disabled",
            SandboxKind::Bubblewrap => "Bubblewrap",
        };
        self.document
            .as_table_mut()
            .entry("sandbox")
            .or_insert_with(toml_edit::table)
            .as_table_mut()
            .ok_or_else(|| anyhow!("sandbox isn't a table"))?
            .insert("kind", toml_edit::value(sandbox_kind));
        Ok(())
    }
}

fn edits_for_build_instruction(
    failure: &crate::problem::DisallowedBuildInstruction,
) -> Vec<Box<dyn Edit>> {
    let mut out: Vec<Box<dyn Edit>> = Vec::new();
    let mut instruction = failure.instruction.as_str();
    let mut suffix = "";
    loop {
        out.push(Box::new(AllowBuildInstruction {
            pkg_name: failure.pkg_name.clone(),
            instruction: format!("{instruction}{suffix}"),
        }));
        suffix = "*";
        let mut last_separator = None;
        instruction = &instruction[..instruction.len() - 1];
        for (pos, ch) in instruction.char_indices() {
            if ch == '=' || ch == '-' || ch == ':' {
                last_separator = Some(pos);
                // After =, don't look for any more separators.
                if ch == '=' {
                    break;
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

struct AllowApiUsage {
    usage: DisallowedApiUsage,
}

impl Edit for AllowApiUsage {
    fn title(&self) -> String {
        let mut sorted_keys: Vec<_> = self.usage.usages.keys().map(|u| u.to_string()).collect();
        sorted_keys.sort();
        format!(
            "Allow `{}` to use APIs: {}",
            self.usage.pkg_name,
            sorted_keys.join(", ")
        )
    }

    fn apply(&self, editor: &mut ConfigEditor) -> Result<()> {
        let table = editor.table(&self.usage.pkg_name)?;
        let allow_apis = table
            .entry("allow_apis")
            .or_insert_with(create_array)
            .as_array_mut()
            .ok_or_else(|| anyhow!("pkg.{}.allow_apis should be an array", self.usage.pkg_name))?;
        let mut sorted_keys: Vec<_> = self.usage.usages.keys().collect();
        sorted_keys.sort();
        for api in sorted_keys {
            allow_apis.push_formatted(create_string(api.to_string()));
        }
        allow_apis.set_trailing("\n");
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

    fn apply(&self, editor: &mut ConfigEditor) -> Result<()> {
        let table = editor.table(&self.unused.pkg_name)?;
        let allow_apis = table
            .entry("allow_apis")
            .or_insert_with(create_array)
            .as_array_mut()
            .ok_or_else(|| anyhow!("pkg.{}.allow_apis should be an array", self.unused.pkg_name))?;
        for api in &self.unused.permissions {
            let index_and_entry = allow_apis
                .iter()
                .enumerate()
                .find(|(_, allowed)| allowed.as_str() == Some(api.to_string().as_str()));
            if let Some((index, _)) = index_and_entry {
                allow_apis.remove(index);
            }
        }
        Ok(())
    }
}

fn create_string(value: String) -> Value {
    Value::String(Formatted::new(value)).decorated("\n    ", "")
}

struct AllowProcMacro {
    pkg_name: String,
}

impl Edit for AllowProcMacro {
    fn title(&self) -> String {
        format!("Allow proc macro `{}`", self.pkg_name)
    }

    fn apply(&self, editor: &mut ConfigEditor) -> Result<()> {
        let table = editor.table(&self.pkg_name)?;
        table["allow_proc_macro"] = toml_edit::value(true);
        Ok(())
    }
}

struct AllowBuildInstruction {
    pkg_name: String,
    instruction: String,
}

impl Edit for AllowBuildInstruction {
    fn title(&self) -> String {
        format!(
            "Allow build script for `{}` to emit instruction `{}`",
            self.pkg_name, self.instruction
        )
    }

    fn apply(&self, editor: &mut ConfigEditor) -> Result<()> {
        let table = editor.table(&format!("{}.build", self.pkg_name))?;
        let allowed = table
            .entry("allow_build_instructions")
            .or_insert_with(create_array)
            .as_array_mut()
            .ok_or_else(|| anyhow!("allow_build_instructions must be an array"))?;
        allowed.push_formatted(create_string(self.instruction.clone()));
        Ok(())
    }
}

struct DisableSandbox {
    pkg_name: String,
}

impl Edit for DisableSandbox {
    fn title(&self) -> String {
        format!("Disable sandbox for `{}`", self.pkg_name)
    }

    fn apply(&self, editor: &mut ConfigEditor) -> Result<()> {
        let table = editor.table(&format!("{}.build.sandbox", self.pkg_name))?;
        table["kind"] = toml_edit::value("Disabled");
        Ok(())
    }
}

struct AllowUnsafe {
    pkg_name: String,
}

impl Edit for AllowUnsafe {
    fn title(&self) -> String {
        format!("Allow package `{}` to use unsafe code", self.pkg_name)
    }

    fn apply(&self, editor: &mut ConfigEditor) -> Result<()> {
        let table = editor.table(&self.pkg_name)?;
        table["allow_unsafe"] = toml_edit::value(true);
        Ok(())
    }
}

struct SandboxAllowNetwork {
    pkg_name: String,
}

impl Edit for SandboxAllowNetwork {
    fn title(&self) -> String {
        format!("Permit network from sandbox for `{}`", self.pkg_name)
    }

    fn apply(&self, editor: &mut ConfigEditor) -> Result<()> {
        let table = editor.table(&format!("{}.build.sandbox", self.pkg_name))?;
        table["allow_network"] = toml_edit::value(true);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::ConfigEditor;
    use crate::config::PermissionName;
    use crate::config::SandboxConfig;
    use crate::config_editor::fixes_for_problem;
    use crate::problem::DisallowedApiUsage;
    use crate::problem::DisallowedBuildInstruction;
    use crate::problem::Problem;
    use crate::proxy::errors::UnsafeUsage;
    use crate::proxy::rpc::BuildScriptOutput;
    use indoc::indoc;

    fn disallowed_apis(pkg_name: &str, apis: &[&'static str]) -> Problem {
        Problem::DisallowedApiUsage(DisallowedApiUsage {
            pkg_name: pkg_name.to_owned(),
            usages: apis
                .iter()
                .map(|n| (PermissionName::from(*n), vec![]))
                .collect(),
        })
    }

    #[track_caller]
    fn check(initial_config: &str, problems: &[(usize, Problem)], expected: &str) {
        let mut editor = ConfigEditor::from_toml_string(initial_config).unwrap();
        for (index, problem) in problems {
            let edit = &fixes_for_problem(problem)[*index];
            edit.apply(&mut editor).unwrap();
        }
        assert_eq!(editor.to_toml(), expected);
    }

    #[test]
    fn fix_missing_api_no_existing_config() {
        check(
            "",
            &[(0, disallowed_apis("crab1", &["fs", "net"]))],
            indoc! {r#"
                [pkg.crab1]
                allow_apis = [
                    "fs",
                    "net",
                ]
            "#,
            },
        );
    }

    #[test]
    fn fix_missing_api_build_script() {
        check(
            "",
            &[(0, disallowed_apis("crab1.build", &["fs", "net"]))],
            indoc! {r#"
                [pkg.crab1.build]
                allow_apis = [
                    "fs",
                    "net",
                ]
            "#,
            },
        );
    }

    #[test]
    fn fix_disallowed_build_instruction() {
        check(
            "",
            &[(
                2,
                Problem::DisallowedBuildInstruction(DisallowedBuildInstruction {
                    pkg_name: "crab1".to_owned(),
                    instruction: "cargo:rustc-link-search=/home/some-path".to_owned(),
                }),
            )],
            indoc! {r#"
                [pkg.crab1.build]
                allow_build_instructions = [
                    "cargo:rustc-link-*",
                ]
            "#,
            },
        );
    }

    #[test]
    fn fix_missing_api_existing_config() {
        check(
            indoc! {r#"
                [pkg.crab1]
                allow_apis = [
                    "env",
                    "fs",
                ]
            "#},
            &[(0, disallowed_apis("crab1", &["net"]))],
            indoc! {r#"
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
    fn fix_allow_proc_macro() {
        check(
            "",
            &[(0, Problem::IsProcMacro("crab1".to_owned()))],
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
            &[(
                0,
                Problem::DisallowedUnsafe(crate::proxy::rpc::UnsafeUsage {
                    crate_name: "crab1".to_owned(),
                    error_info: UnsafeUsage {
                        file_name: "main.rs".into(),
                        start_line: 10,
                    },
                }),
            )],
            indoc! {r#"
                [pkg.crab1]
                allow_unsafe = true
            "#,
            },
        );
    }

    #[test]
    fn build_script_failed() {
        let failure = Problem::BuildScriptFailed(crate::problem::BuildScriptFailed {
            output: BuildScriptOutput {
                exit_code: 1,
                stdout: Vec::new(),
                stderr: Vec::new(),
                package_name: "crab1".to_owned(),
                sandbox_config: SandboxConfig {
                    kind: crate::config::SandboxKind::Bubblewrap,
                    allow_read: vec![],
                    extra_args: vec![],
                    allow_network: None,
                },
                build_script: PathBuf::new(),
            },
        });
        check(
            "",
            &[(0, failure.clone())],
            indoc! {r#"
                [pkg.crab1.build.sandbox]
                kind = "Disabled"
            "#,
            },
        );
        check(
            "",
            &[(1, failure)],
            indoc! {r#"
                [pkg.crab1.build.sandbox]
                allow_network = true
            "#,
            },
        );
    }

    #[test]
    fn unused_allow_api() {
        let failure = Problem::UnusedAllowApi(crate::problem::UnusedAllowApi {
            pkg_name: "crab1.build".to_owned(),
            permissions: vec![PermissionName::new("fs"), PermissionName::new("net")],
        });
        check(
            indoc! {r#"
                [pkg.crab1.build]
                allow_apis = [
                    "fs",
                    "env",
                    "net",
                ]
            "#},
            &[(0, failure)],
            indoc! {r#"
                [pkg.crab1.build]
                allow_apis = [
                    "env",
                ]
            "#,
            },
        );
    }
}
