use crate::config::Config;
use crate::config::PackageConfig;
use crate::config::DEFAULT_PACKAGE_CONFIG;
use crate::problem::Problem;
use crate::problem::Problems;
use crate::proxy::rpc::BuildScriptOutput;

pub(crate) fn check(outputs: &BuildScriptOutput, config: &Config) -> Problems {
    let pkg_name = &outputs.package_name;
    let crate_name = format!("{}.build", outputs.package_name);
    let pkg_config = config
        .packages
        .get(&crate_name)
        .unwrap_or(&DEFAULT_PACKAGE_CONFIG);
    let Ok(stdout) = std::str::from_utf8(&outputs.stdout) else {
        return Problem::new(format!("The package `{pkg_name}`'s build script emitted invalid UTF-8")).into();
    };
    let mut problems = Problems::default();
    for line in stdout.lines() {
        if line.starts_with("cargo:") {
            problems.merge(check_directive(line, &pkg_name, &pkg_config));
        }
    }
    problems
}

/// Cargo instructions that should be harmless, so would just add noise if we were required to
/// explicitly allow them.
const ALWAYS_PERMITTED: &[&str] = &["cargo:rerun-if-", "cargo:warning", "cargo:rustc-cfg="];

fn check_directive(instruction: &str, pkg_name: &str, config: &PackageConfig) -> Problems {
    if ALWAYS_PERMITTED
        .iter()
        .any(|prefix| instruction.starts_with(prefix))
    {
        return Problems::default();
    }
    if config
        .allow_build_instructions
        .iter()
        .any(|i| matches(instruction, i))
    {
        return Problems::default();
    }
    Problem::Message(format!(
        "{pkg_name}'s build script emitted disallowed instruction `{instruction}`"
    ))
    .into()
}

fn matches(instruction: &str, rule: &str) -> bool {
    if let Some(prefix) = rule.strip_suffix("*") {
        instruction.starts_with(prefix)
    } else {
        instruction == rule
    }
}

#[cfg(test)]
mod tests {
    use crate::config;
    use crate::problem::Problem;
    use crate::problem::Problems;
    use crate::proxy::rpc::BuildScriptOutput;

    #[track_caller]
    fn check(stdout: &str, config_str: &str) -> Problems {
        let config = config::testing::parse(config_str).unwrap();
        let outputs = BuildScriptOutput {
            stdout: stdout.as_bytes().to_owned(),
            stderr: vec![],
            package_name: "my_pkg".to_owned(),
        };
        super::check(&outputs, &config)
    }

    #[test]
    fn test_empty() {
        assert_eq!(check("", ""), Problems::default());
    }

    #[test]
    fn test_rerun_if_changed() {
        assert_eq!(
            check("cargo:rerun-if-changed=a.txt", ""),
            Problems::default()
        );
    }

    #[test]
    fn test_link_directive() {
        assert_eq!(
            check("cargo:rustc-link-search=some_directory", ""),
            Problem::new(
                "my_pkg's build script emitted disallowed instruction `cargo:rustc-link-search=some_directory`"
            )
            .into()
        );
        assert_eq!(
            check(
                "cargo:rustc-link-search=some_directory",
                r#"
                [pkg.my_pkg.build]
                allow_build_instructions = [ "cargo:rustc-link-search=some_directory" ]
                "#
            ),
            Problems::default()
        );
        assert_eq!(
            check(
                "cargo:rustc-link-search=some_directory",
                r#"
                [pkg.my_pkg.build]
                allow_build_instructions = [ "cargo:rustc-link-*" ]
                "#
            ),
            Problems::default()
        );
    }
}
