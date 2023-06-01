//! This module is responsible for applying automatic edits to cackle.toml.

use crate::config::SandboxKind;
use crate::problem::DisallowedApiUsage;
use crate::problem::Problem;
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
        _ => {}
    }
    edits
}

impl ConfigEditor {
    pub(crate) fn from_file(filename: &Path) -> Result<Self> {
        let toml = std::fs::read_to_string(filename)?;
        Self::from_toml_string(&toml)
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
}

fn create_array() -> Item {
    let mut array = Array::new();
    array.set_trailing_comma(true);
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
            let value = Value::String(Formatted::new(api.to_string()));
            allow_apis.push_formatted(value.decorated("\n    ", ""));
        }
        allow_apis.set_trailing("\n");
        Ok(())
    }
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

struct DisableSandbox {
    pkg_name: String,
}

impl Edit for DisableSandbox {
    fn title(&self) -> String {
        format!("Disable sandbox for `{}`", self.pkg_name)
    }

    fn apply(&self, editor: &mut ConfigEditor) -> Result<()> {
        let table = editor.table(&format!("{}.sandbox", self.pkg_name))?;
        table["kind"] = toml_edit::value("disabled");
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
        let table = editor.table(&format!("{}.sandbox", self.pkg_name))?;
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
    use crate::problem::Problem;
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
    fn build_script_failed() {
        let failure = Problem::BuildScriptFailed(crate::problem::BuildScriptFailed {
            output: BuildScriptOutput {
                exit_code: 1,
                stdout: Vec::new(),
                stderr: Vec::new(),
                package_name: "crab1.build".to_owned(),
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
                kind = "disabled"
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
}
