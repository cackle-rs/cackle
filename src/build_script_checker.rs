use crate::config::permissions::PermSel;
use crate::config::Config;
use crate::crate_index::PackageId;
use crate::problem::DisallowedBuildInstruction;
use crate::problem::Problem;
use crate::problem::ProblemList;
use crate::proxy::rpc::BinExecutionOutput;
use anyhow::Result;

pub(crate) fn check(outputs: &BinExecutionOutput, config: &Config) -> Result<ProblemList> {
    let crate_sel = &outputs.crate_sel;
    if outputs.exit_code != 0 {
        return Ok(
            Problem::BuildScriptFailed(crate::problem::BinExecutionFailed {
                output: outputs.clone(),
                crate_sel: crate_sel.clone(),
            })
            .into(),
        );
    }
    let perm_sel = PermSel::for_build_script(crate_sel.pkg_name());
    let allow_build_instructions = config
        .permissions
        .get(&perm_sel)
        .map(|cfg| cfg.allow_build_instructions.as_slice())
        .unwrap_or(&[]);
    let Ok(stdout) = std::str::from_utf8(&outputs.stdout) else {
        return Ok(Problem::new(format!(
            "The build script `{}` emitted invalid UTF-8",
            crate_sel.pkg_id
        ))
        .into());
    };
    let mut problems = ProblemList::default();
    for line in stdout.lines() {
        if line.starts_with("cargo:") {
            problems.merge(check_directive(
                line,
                &crate_sel.pkg_id,
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
    pkg_id: &PackageId,
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
        pkg_id: pkg_id.clone(),
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
    use crate::crate_index::testing::pkg_id;
    use crate::crate_index::CrateSel;
    use crate::problem::DisallowedBuildInstruction;
    use crate::problem::Problem;
    use crate::problem::ProblemList;
    use crate::proxy::rpc::BinExecutionOutput;
    use std::path::PathBuf;

    #[track_caller]
    fn check(stdout: &str, config_str: &str) -> ProblemList {
        let config = config::testing::parse(config_str).unwrap();
        let outputs = BinExecutionOutput {
            exit_code: 0,
            stdout: stdout.as_bytes().to_owned(),
            stderr: vec![],
            crate_sel: CrateSel::build_script(pkg_id("my_pkg")),
            sandbox_config: SandboxConfig::default(),
            build_script: PathBuf::new(),
            sandbox_config_display: None,
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
                pkg_id: pkg_id("my_pkg"),
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
