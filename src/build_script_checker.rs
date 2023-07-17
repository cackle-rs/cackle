use crate::config::Config;
use crate::config::CrateName;
use crate::crate_index::BuildScriptId;
use crate::problem::DisallowedBuildInstruction;
use crate::problem::Problem;
use crate::problem::ProblemList;
use crate::proxy::rpc::BuildScriptOutput;
use anyhow::Result;

pub(crate) fn check(outputs: &BuildScriptOutput, config: &Config) -> Result<ProblemList> {
    let build_script_id = &outputs.build_script_id;
    if outputs.exit_code != 0 {
        return Ok(
            Problem::BuildScriptFailed(crate::problem::BuildScriptFailed {
                output: outputs.clone(),
                build_script_id: build_script_id.clone(),
            })
            .into(),
        );
    }
    let crate_name = CrateName::from(build_script_id);
    let allow_build_instructions = config
        .packages
        .get(&crate_name)
        .map(|cfg| cfg.allow_build_instructions.as_slice())
        .unwrap_or(&[]);
    let Ok(stdout) = std::str::from_utf8(&outputs.stdout) else {
        return Ok(Problem::new(format!(
            "The build script `{}` emitted invalid UTF-8",
            build_script_id
        ))
        .into());
    };
    let mut problems = ProblemList::default();
    for line in stdout.lines() {
        if line.starts_with("cargo:") {
            problems.merge(check_directive(
                line,
                build_script_id,
                allow_build_instructions,
            ));
        }
    }
    Ok(problems)
}

/// Cargo instructions that should be harmless, so would just add noise if we were required to
/// explicitly allow them.
const ALWAYS_PERMITTED: &[&str] = &["cargo:rerun-if-", "cargo:warning", "cargo:rustc-cfg="];

fn check_directive(
    instruction: &str,
    build_script_id: &BuildScriptId,
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
        build_script_id: build_script_id.clone(),
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
    use crate::config;
    use crate::config::SandboxConfig;
    use crate::crate_index::testing::build_script_id;
    use crate::problem::DisallowedBuildInstruction;
    use crate::problem::Problem;
    use crate::problem::ProblemList;
    use crate::proxy::rpc::BuildScriptOutput;
    use std::path::PathBuf;

    #[track_caller]
    fn check(stdout: &str, config_str: &str) -> ProblemList {
        let config = config::testing::parse(config_str).unwrap();
        let outputs = BuildScriptOutput {
            exit_code: 0,
            stdout: stdout.as_bytes().to_owned(),
            stderr: vec![],
            build_script_id: build_script_id("my_pkg"),
            sandbox_config: SandboxConfig::default(),
            build_script: PathBuf::new(),
        };
        super::check(&outputs, &config).unwrap()
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
                build_script_id: build_script_id("my_pkg"),
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
