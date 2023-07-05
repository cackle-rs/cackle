use crate::config::Config;
use crate::config::CrateName;
use crate::problem::DisallowedBuildInstruction;
use crate::problem::Problem;
use crate::problem::ProblemList;
use crate::proxy::rpc::BuildScriptOutput;

pub(crate) fn check(outputs: &BuildScriptOutput, config: &Config) -> ProblemList {
    if outputs.exit_code != 0 {
        return Problem::BuildScriptFailed(crate::problem::BuildScriptFailed {
            output: outputs.clone(),
        })
        .into();
    }
    let crate_name = &outputs.crate_name;
    let allow_build_instructions = config
        .packages
        .get(crate_name)
        .map(|cfg| cfg.allow_build_instructions.as_slice())
        .unwrap_or(&[]);
    let Ok(stdout) = std::str::from_utf8(&outputs.stdout) else {
        return Problem::new(format!(
            "The build script `{crate_name}` emitted invalid UTF-8"
        ))
        .into();
    };
    let mut problems = ProblemList::default();
    for line in stdout.lines() {
        if line.starts_with("cargo:") {
            problems.merge(check_directive(line, crate_name, allow_build_instructions));
        }
    }
    problems
}

/// Cargo instructions that should be harmless, so would just add noise if we were required to
/// explicitly allow them.
const ALWAYS_PERMITTED: &[&str] = &["cargo:rerun-if-", "cargo:warning", "cargo:rustc-cfg="];

fn check_directive(
    instruction: &str,
    crate_name: &CrateName,
    allow_build_instructions: &[String],
) -> ProblemList {
    if ALWAYS_PERMITTED
        .iter()
        .any(|prefix| instruction.starts_with(prefix))
    {
        return ProblemList::default();
    }
    if allow_build_instructions
        .iter()
        .any(|i| matches(instruction, i))
    {
        return ProblemList::default();
    }
    Problem::DisallowedBuildInstruction(DisallowedBuildInstruction {
        crate_name: crate_name.to_owned(),
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
    use crate::config::CrateName;
    use crate::config::SandboxConfig;
    use crate::problem::DisallowedBuildInstruction;
    use crate::problem::Problem;
    use crate::problem::ProblemList;
    use crate::proxy::rpc::BuildScriptOutput;

    #[track_caller]
    fn check(stdout: &str, config_str: &str) -> ProblemList {
        let config = config::testing::parse(config_str).unwrap();
        let outputs = BuildScriptOutput {
            exit_code: 0,
            stdout: stdout.as_bytes().to_owned(),
            stderr: vec![],
            crate_name: CrateName::for_build_script("my_pkg"),
            sandbox_config: SandboxConfig::default(),
            build_script: PathBuf::new(),
        };
        super::check(&outputs, &config)
    }

    #[test]
    fn test_empty() {
        assert_eq!(check("", ""), ProblemList::default());
    }

    #[test]
    fn test_rerun_if_changed() {
        assert_eq!(
            check("cargo:rerun-if-changed=a.txt", ""),
            ProblemList::default()
        );
    }

    #[test]
    fn test_link_directive() {
        assert_eq!(
            check("cargo:rustc-link-search=some_directory", ""),
            Problem::DisallowedBuildInstruction(DisallowedBuildInstruction {
                crate_name: CrateName::for_build_script("my_pkg"),
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
            ProblemList::default()
        );
        assert_eq!(
            check(
                "cargo:rustc-link-search=some_directory",
                r#"
                [pkg.my_pkg.build]
                allow_build_instructions = [ "cargo:rustc-link-*" ]
                "#
            ),
            ProblemList::default()
        );
    }
}
