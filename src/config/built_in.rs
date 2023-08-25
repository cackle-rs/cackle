use super::ApiConfig;
use super::ApiName;
use super::ApiPath;
use std::collections::BTreeMap;

pub(crate) fn get_built_ins() -> BTreeMap<ApiName, ApiConfig> {
    let mut result = BTreeMap::new();
    result.insert(
        ApiName::from("fs"),
        perm(
            &[
                // std::env provides quite a few functions that return paths, which can in turn
                // allow filesystem access.
                "std::env",
                "std::fs",
                "std::os::linux::fs",
                "std::os::unix::fs",
                "std::os::unix::io",
                "std::os::wasi::fs",
                "std::os::wasi::io",
                "std::os::windows::fs",
                "std::os::windows::io",
                "std::path",
            ],
            &[
                "std::env::Args",
                "std::env::ArgsOs",
                "std::env::VarError",
                "std::env::_var",
                "std::env::_var_os",
                "std::env::args",
                "std::env::args_os",
                "std::env::var",
                "std::env::var_os",
                "std::env::vars",
                "std::env::vars_os",
            ],
        ),
    );
    result.insert(ApiName::from("env"), perm(&["std::env"], &[]));
    result.insert(
        ApiName::from("net"),
        perm(
            &["std::net", "std::os::wasi::net", "std::os::windows::net"],
            &[],
        ),
    );
    result.insert(
        ApiName::from("unix_sockets"),
        perm(&["std::os::unix::net"], &[]),
    );
    result.insert(
        ApiName::from("process"),
        perm(
            &[
                "std::process",
                "std::unix::process",
                "std::windows::process",
            ],
            &["std::process::abort", "std::process::exit"],
        ),
    );
    result.insert(
        ApiName::from("terminate"),
        perm(&["std::process::abort", "std::process::exit"], &[]),
    );
    result
}

fn perm(include: &[&str], exclude: &[&str]) -> ApiConfig {
    ApiConfig {
        include: include.iter().map(|s| ApiPath::from_str(s)).collect(),
        exclude: exclude.iter().map(|s| ApiPath::from_str(s)).collect(),
        no_auto_detect: Vec::new(),
    }
}
