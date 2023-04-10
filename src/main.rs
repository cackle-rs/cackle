#![forbid(unsafe_code)]

// TODO: Search for all uses of #[allow(dead_code)] and remove.

mod built_in_perms;
mod checker;
mod config;
mod config_validation;
mod crate_paths;
mod proxy;
#[allow(dead_code)]
mod sandbox;
pub(crate) mod symbol;
mod symbol_graph;

use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use checker::Checker;
use clap::Parser;
use clap::Subcommand;
use config::Config;
use crate_paths::SourceMapping;
use proxy::rpc::CanContinueResponse;
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

    /// Path to Cackle.toml. If not specified, looks in the directory containing
    /// the crate to be analyzed.
    #[clap(short, long)]
    cackle: Option<PathBuf>,

    #[clap(subcommand)]
    command: Command,

    /// Print all references (may be large). Useful for debugging why something is passing when you
    /// think it shouldn't be.
    #[clap(long)]
    print_all_references: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    Usage,
    ShowReferences,
    Check(CheckConfig),
}

#[derive(Parser, Debug)]
struct CheckConfig {
    /// Maximum number of source locations that use an API that should be
    /// reported.
    #[clap(long, default_value = "2")]
    usage_report_cap: i32,
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
        .cackle
        .clone()
        .unwrap_or_else(|| root_path.join("Cackle.toml"));

    let config = config::parse_file(&config_path).context("Invalid config file")?;

    let mut cackle = Cackle::new(config, &root_path, args)?;
    let build_result = proxy::invoke_cargo_build(&root_path, &config_path, |request| {
        cackle.check_acls(request)
    });
    if !cackle.errors.is_empty() {
        for error in cackle.errors {
            println!("{error:?}");
        }
        std::process::exit(1);
    }
    // We only check if the build failed if there were no ACL check errors.
    build_result.context("Cargo build failed")?;

    if let Err(unused) = cackle.checker.check_unused() {
        println!("{}", unused);
    }

    println!("Cackle done");

    Ok(())
}

struct Cackle {
    checker: Checker,
    errors: Vec<anyhow::Error>,
    target_dir: PathBuf,
    source_mapping: SourceMapping,
    args: Args,
}

impl Cackle {
    fn new(config: Config, root_path: &Path, args: Args) -> Result<Self> {
        let mut checker = Checker::from_config(&config);
        let source_mapping = SourceMapping::new(root_path)?;
        for crate_name in source_mapping.crate_names() {
            let crate_id = checker.crate_id_from_name(crate_name);
            checker.report_crate_used(crate_id);
        }
        Ok(Self {
            checker,
            errors: Vec::new(),
            target_dir: root_path.join("target"),
            source_mapping,
            args,
        })
    }

    fn check_acls(&mut self, request: proxy::rpc::Request) -> CanContinueResponse {
        match request {
            proxy::rpc::Request::CrateUsesUnsafe(usage) => {
                self.errors.push(anyhow!(
                    "Crate {} uses unsafe at {}:{} and doesn't have `allow_unsafe = true`",
                    usage.crate_name,
                    usage.error_info.file_name,
                    usage.error_info.start_line
                ));
                CanContinueResponse::Deny
            }
            proxy::rpc::Request::LinkerArgs(linker_args) => {
                match self.check_link_args(&linker_args) {
                    Ok(response) => response,
                    Err(error) => {
                        self.errors.push(error);
                        CanContinueResponse::Deny
                    }
                }
            }
        }
    }

    fn check_link_args(&mut self, linker_args: &[String]) -> Result<CanContinueResponse> {
        let mut graph = SymGraph::default();
        for arg in linker_args {
            let path = Path::new(arg);
            if self.should_check(path) {
                graph
                    .process_file(Path::new(arg))
                    .with_context(|| format!("Failed to process `{arg}`"))?;
            }
        }
        graph.apply_to_checker(&mut self.checker, &self.source_mapping)?;
        let mut can_proceed = self.checker.report_problems(&CheckConfig {
            usage_report_cap: 2,
        });
        if self.args.print_all_references {
            println!("{graph}");
        }
        if let Err(error) = graph.validate() {
            // TODO: Decide if we're printing errors to stdout or stderr and make sure we're
            // consistent.
            println!("{error}");
            can_proceed = CanContinueResponse::Deny;
        }
        //graph.print_stats();
        Ok(can_proceed)
    }

    fn should_check(&self, path: &Path) -> bool {
        if !self.has_supported_extension(path) {
            return false;
        }
        path.canonicalize()
            .map(|path| path.starts_with(&self.target_dir))
            .unwrap_or(false)
    }

    fn has_supported_extension(&self, path: &Path) -> bool {
        const EXTENSIONS: &[&str] = &["rlib", "o"];
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| EXTENSIONS.contains(&ext))
            .unwrap_or(false)
    }
}
