#![forbid(unsafe_code)]

// TODO: Search for all uses of #[allow(dead_code)] and remove.

mod built_in_perms;
mod checker;
mod config;
mod config_validation;
pub(crate) mod link_info;
pub(crate) mod problem;
mod proxy;
#[allow(dead_code)]
mod sandbox;
pub(crate) mod section_name;
mod source_mapping;
pub(crate) mod symbol;
mod symbol_graph;

use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use checker::Checker;
use clap::Parser;
use config::Config;
use link_info::LinkInfo;
use problem::Problem;
use problem::Problems;
use source_mapping::SourceMapping;
use std::path::Path;
use std::path::PathBuf;
use symbol_graph::SymGraph;

/// Analyses rust crates and their dependent crates to see what categories of
/// APIs and language features are used.
#[derive(Parser, Debug)]
#[clap(version, about, long_about = None)]
struct Args {
    /// Directory containing crate to analyze. Defaults to current working
    /// directory.
    #[clap(short, long)]
    path: Option<PathBuf>,

    /// Path to cackle.toml. If not specified, looks in the directory containing
    /// the crate to be analyzed.
    #[clap(short, long)]
    cackle_path: Option<PathBuf>,

    /// Print all references (may be large). Useful for debugging why something is passing when you
    /// think it shouldn't be.
    #[clap(long)]
    print_all_references: bool,

    /// Print the mapping from paths to crate names. Useful for debugging.
    #[clap(long)]
    print_path_to_crate_map: bool,

    /// Maximum number of source locations that use an API that should be
    /// reported.
    #[clap(long, default_value = "2")]
    usage_report_cap: i32,

    /// Analyse specified object file(s). Useful for debugging.
    #[clap(long, num_args = 1.., value_delimiter = ' ')]
    object_paths: Vec<PathBuf>,
}

fn main() -> Result<()> {
    proxy::handle_wrapped_binaries()?;

    let args = Args::parse();

    let root_path = args
        .path
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| anyhow!("Failed to get current working directory"))?;
    let root_path = Path::new(&root_path)
        .canonicalize()
        .with_context(|| format!("Failed to read directory `{}`", root_path.display()))?;

    let config_path = args
        .cackle_path
        .clone()
        .unwrap_or_else(|| root_path.join("cackle.toml"));

    let config = config::parse_file(&config_path).context("Invalid config file")?;

    let mut cackle = Cackle::new(config, &root_path, args)?;

    if !cackle.args.object_paths.is_empty() {
        let paths: Vec<_> = cackle.args.object_paths.clone();
        cackle.check_object_paths(&paths)?;
        return Ok(());
    }

    let mut problems = Problems::default();
    let build_result = proxy::invoke_cargo_build(&root_path, &config_path, |request| {
        problems.merge(cackle.check_acls(request));
        problems.can_continue()
    });

    if !problems.is_empty() {
        for problem in problems {
            println!("{problem}");
        }
        std::process::exit(1);
    }
    // We only check if the build failed if there were no ACL check errors.
    build_result.context("Cargo build failed")?;

    if let Err(unused) = cackle.checker.check_unused() {
        println!("{}", unused);
    }

    println!("Cackle succcess");

    Ok(())
}

struct Cackle {
    checker: Checker,
    target_dir: PathBuf,
    source_mapping: SourceMapping,
    args: Args,
}

impl Cackle {
    fn new(config: Config, root_path: &Path, args: Args) -> Result<Self> {
        let mut checker = Checker::from_config(&config);
        let source_mapping = SourceMapping::new(root_path)?;
        if args.print_path_to_crate_map {
            println!("{source_mapping}");
        }
        for crate_name in source_mapping.crate_names() {
            let crate_id = checker.crate_id_from_name(crate_name);
            checker.report_crate_used(crate_id);
        }
        Ok(Self {
            checker,
            target_dir: root_path.join("target"),
            source_mapping,
            args,
        })
    }

    fn check_acls(&mut self, request: proxy::rpc::Request) -> Problems {
        match request {
            proxy::rpc::Request::CrateUsesUnsafe(usage) => {
                return Problem::new(format!(
                    "Crate {} uses unsafe at {}:{} and doesn't have `allow_unsafe = true`",
                    usage.crate_name, usage.error_info.file_name, usage.error_info.start_line
                ))
                .into();
            }
            proxy::rpc::Request::LinkerInvoked(link_info) => {
                self.check_linker_invocation(&link_info).into()
            }
        }
    }

    fn check_linker_invocation(&mut self, info: &LinkInfo) -> Result<Problems> {
        if info.is_build_script {
            self.checker
                .verify_build_script_permitted(&info.package_name)?;
        }
        self.check_object_paths(&info.object_paths_under(&self.target_dir))
    }

    fn check_object_paths(&mut self, paths: &[PathBuf]) -> Result<Problems> {
        let mut graph = SymGraph::default();
        for path in paths {
            graph
                .process_file(path)
                .with_context(|| format!("Failed to process `{}`", path.display()))?;
        }
        graph.apply_to_checker(&mut self.checker, &self.source_mapping)?;
        let mut problems = self.checker.problems(&self.args);
        if self.args.print_all_references {
            println!("{graph}");
        }
        problems.merge(graph.validate());
        Ok(problems)
    }
}
