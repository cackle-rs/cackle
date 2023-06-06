use crate::config::Config;
use crate::problem::DisallowedBuildInstruction;
use crate::problem::Problem;
use crate::problem::Problems;
use crate::proxy::rpc::BuildScriptOutput;

pub(crate) fn check(outputs: &BuildScriptOutput, config: &Config) -> Problems {
    if outputs.exit_code != 0 {
        return Problem::BuildScriptFailed(crate::problem::BuildScriptFailed {
            output: outputs.clone(),
        })
        .into();
    }
    let pkg_name = &outputs.package_name;
    let crate_name = format!("{}.build", outputs.package_name);
    let allow_build_instructions = config
        .packages
        .get(&crate_name)
        .map(|cfg| cfg.allow_build_instructions.as_slice())
        .unwrap_or(&[]);
    let Ok(stdout) = std::str::from_utf8(&outputs.stdout) else {
        return Problem::new(format!("The package `{pkg_name}`'s build script emitted invalid UTF-8")).into();
    };
    let mut problems = Problems::default();
    for line in stdout.lines() {
        if line.starts_with("cargo:") {
            problems.merge(check_directive(line, pkg_name, allow_build_instructions));
        }
    }
    problems
}

/// Cargo instructions that should be harmless, so would just add noise if we were required to
/// explicitly allow them.
const ALWAYS_PERMITTED: &[&str] = &["cargo:rerun-if-", "cargo:warning", "cargo:rustc-cfg="];

fn check_directive(
    instruction: &str,
    pkg_name: &str,
    allow_build_instructions: &[String],
) -> Problems {
    if ALWAYS_PERMITTED
        .iter()
        .any(|prefix| instruction.starts_with(prefix))
    {
        return Problems::default();
    }
    if allow_build_instructions
        .iter()
        .any(|i| matches(instruction, i))
    {
        return Problems::default();
    }
    Problem::DisallowedBuildInstruction(DisallowedBuildInstruction {
        pkg_name: pkg_name.to_owned(),
        instruction: instruction.to_owned(),
    })
    .into()
}

fn matches(instruction: &str, rule: &str) -> bool {
    if let Some(prefix) = rule.strip_suffix('*') {
        instruction.starts_with(prefix)
    } else {
        instruction == rule
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::config;
    use crate::config::SandboxConfig;
    use crate::problem::DisallowedBuildInstruction;
    use crate::problem::Problem;
    use crate::problem::Problems;
    use crate::proxy::rpc::BuildScriptOutput;

    #[track_caller]
    fn check(stdout: &str, config_str: &str) -> Problems {
        let config = config::testing::parse(config_str).unwrap();
        let outputs = BuildScriptOutput {
            exit_code: 0,
            stdout: stdout.as_bytes().to_owned(),
            stderr: vec![],
            package_name: "my_pkg".to_owned(),
            sandbox_config: SandboxConfig::default(),
            build_script: PathBuf::new(),
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
            Problem::DisallowedBuildInstruction(DisallowedBuildInstruction {
                pkg_name: "my_pkg".to_owned(),
                instruction: "cargo:rustc-link-search=some_directory".to_owned(),
            })
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
