//! This module is responsible for applying automatic edits to cackle.toml.

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
    match problem {
        Problem::DisallowedApiUsage(usage) => {
            vec![Box::new(AllowApiUsage {
                usage: usage.clone(),
            })]
        }
        Problem::IsProcMacro(pkg_name) => {
            vec![Box::new(AllowProcMacro {
                pkg_name: pkg_name.clone(),
            })]
        }
        _ => vec![],
    }
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

    fn pkg_table(&mut self, pkg_name: &str) -> Result<&mut toml_edit::Table> {
        let pkg = self
            .document
            .as_table_mut()
            .entry("pkg")
            .or_insert_with(create_implicit_table)
            .as_table_mut()
            .ok_or_else(|| anyhow!("[pkg] should be a table"))?;
        pkg.entry(pkg_name)
            .or_insert_with(toml_edit::table)
            .as_table_mut()
            .ok_or_else(|| anyhow!("[pkg.{pkg_name}] should be a table"))
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
        let table = editor.pkg_table(&self.usage.pkg_name)?;
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
        let table = editor.pkg_table(&self.pkg_name)?;
        table["allow_proc_macro"] = toml_edit::value(true);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ConfigEditor;
    use crate::config::PermissionName;
    use crate::config_editor::fixes_for_problem;
    use crate::problem::DisallowedApiUsage;
    use crate::problem::Problem;
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
    fn check(initial_config: &str, problems: &[Problem], expected: &str) {
        let mut editor = ConfigEditor::from_toml_string(initial_config).unwrap();
        for problem in problems {
            for edit in fixes_for_problem(problem) {
                edit.apply(&mut editor).unwrap();
            }
        }
        assert_eq!(editor.to_toml(), expected);
    }

    #[test]
    fn fix_missing_api_no_existing_config() {
        check(
            "",
            &[disallowed_apis("crab1", &["fs", "net"])],
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
    fn fix_missing_api_existing_config() {
        check(
            indoc! {r#"
                [pkg.crab1]
                allow_apis = [
                    "env",
                    "fs",
                ]
            "#},
            &[disallowed_apis("crab1", &["net"])],
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
            &[Problem::IsProcMacro("crab1".to_owned())],
            indoc! {r#"
                [pkg.crab1]
                allow_proc_macro = true
            "#,
            },
        );
    }
}
