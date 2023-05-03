//! Analyses rust crates and their dependent crates to see what categories of APIs and language
//! features are used.

#![forbid(unsafe_code)]

mod build_script_checker;
mod checker;
mod colour;
mod config;
mod config_editor;
mod config_validation;
mod crate_index;
pub(crate) mod link_info;
pub(crate) mod problem;
mod proxy;
mod sandbox;
pub(crate) mod section_name;
pub(crate) mod symbol;
mod symbol_graph;

use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use checker::Checker;
use clap::Parser;
use colored::Colorize;
use config::Config;
use crate_index::CrateIndex;
use link_info::LinkInfo;
use problem::Problem;
use problem::Problems;
use std::path::Path;
use std::path::PathBuf;
use symbol_graph::SymGraph;

use crate::config_editor::ConfigEditor;

#[derive(Parser, Debug)]
#[clap(version, about)]
struct Args {
    /// Directory containing crate to analyze. Defaults to current working
    /// directory.
    #[clap(short, long)]
    path: Option<PathBuf>,

    /// Path to cackle.toml. If not specified, looks in the directory containing
    /// the crate to be analyzed.
    #[clap(short, long)]
    cackle_path: Option<PathBuf>,

    /// If problems are found, don't prompt whether to adjust the configuration.
    #[clap(long)]
    non_interactive: bool,

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

    /// Whether to use coloured output.
    #[clap(long, alias = "color", default_value = "auto")]
    colour: colour::Colour,
}

fn main() -> Result<()> {
    proxy::subprocess::handle_wrapped_binaries()?;

    let mut args = Args::parse();
    args.colour = args.colour.detect();
    if let Err(error) = run(args) {
        println!("{} {:#}", "ERROR:".red(), error);
    }
    Ok(())
}

fn run(args: Args) -> Result<()> {
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

    let mut cackle = Cackle::new(config_path, &root_path, args)?;

    if !cackle.args.object_paths.is_empty() {
        let paths: Vec<_> = cackle.args.object_paths.clone();
        report_problems_and_maybe_exit(&cackle.check_object_paths(&paths)?);
        return Ok(());
    }

    let mut problems = cackle.maybe_fix_problems(cackle.checker.problems())?;
    let config_path = cackle.config_path.clone();
    let build_result = if problems.is_empty() {
        proxy::invoke_cargo_build(&root_path, &config_path, cackle.args.colour, |request| {
            let acl_problems = cackle.check_acls(request);
            problems.merge(cackle.maybe_fix_problems(acl_problems)?);
            Ok(problems.can_continue())
        })
    } else {
        // We've already detected problems before running cargo, don't run cargo.
        Ok(None)
    };

    report_problems_and_maybe_exit(&problems);

    // We only check if the build failed if there were no ACL check errors.
    if let Some(build_failure) = build_result? {
        println!("{build_failure}");
        std::process::exit(-1);
    }

    if !cackle.config.ignore_unused {
        if let Err(unused) = cackle.checker.check_unused() {
            println!("{}", unused);
        }
    }

    println!("Cackle succcess");

    Ok(())
}

fn report_problems_and_maybe_exit(problems: &Problems) {
    if !problems.is_empty() {
        for problem in problems {
            println!("{} {problem}", "ERROR:".red());
        }
        std::process::exit(-1);
    }
}

struct Cackle {
    config_path: PathBuf,
    config: Config,
    checker: Checker,
    target_dir: PathBuf,
    crate_index: CrateIndex,
    args: Args,
}

impl Cackle {
    fn new(config_path: PathBuf, root_path: &Path, args: Args) -> Result<Self> {
        let config = config::parse_file(&config_path)?;
        let mut checker = Checker::from_config(&config);
        let crate_index = CrateIndex::new(root_path)?;
        if args.print_path_to_crate_map {
            println!("{crate_index}");
        }
        for crate_name in crate_index.crate_names() {
            let crate_id = checker.crate_id_from_name(crate_name);
            checker.report_crate_used(crate_id);
        }
        for crate_name in &crate_index.proc_macros {
            let crate_id = checker.crate_id_from_name(crate_name);
            checker.report_proc_macro(crate_id);
        }
        Ok(Self {
            config_path,
            config,
            checker,
            target_dir: root_path.join("target"),
            crate_index,
            args,
        })
    }

    fn maybe_fix_problems(&mut self, problems: Problems) -> Result<Problems> {
        if !self.args.non_interactive && !problems.is_empty() {
            let mut editor = ConfigEditor::from_file(&self.config_path)?;
            editor.fix_problems(&problems)?;
            if editor.has_unsupported {
                return Ok(problems);
            }
            println!("============================================");
            for problem in &problems {
                println!("{problem}");
            }
            println!("Permit the above and continue build? [y/N]");
            let mut response = String::new();
            std::io::stdin().read_line(&mut response)?;
            if response.trim().to_lowercase() == "y" {
                editor.write(&self.config_path)?;
                self.config = config::parse_file(&self.config_path)?;
                self.checker.load_config(&self.config);
                return Ok(Problems::default());
            }
        }
        Ok(problems)
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
            proxy::rpc::Request::BuildScriptComplete(output) => {
                self.check_build_script_output(output)
            }
        }
    }

    fn check_linker_invocation(&mut self, info: &LinkInfo) -> Result<Problems> {
        let mut problems = Problems::default();
        if info.is_build_script {
            problems.merge(
                self.checker
                    .verify_build_script_permitted(&info.package_name),
            );
        }
        problems.merge(self.check_object_paths(&info.object_paths_under(&self.target_dir))?);
        Ok(problems)
    }

    fn check_object_paths(&mut self, paths: &[PathBuf]) -> Result<Problems> {
        let mut graph = SymGraph::default();
        for path in paths {
            graph
                .process_file(path)
                .with_context(|| format!("Failed to process `{}`", path.display()))?;
        }
        graph.apply_to_checker(&mut self.checker, &self.crate_index)?;
        let mut problems = self.checker.problems();
        if self.args.print_all_references {
            println!("{graph}");
        }
        problems.merge(graph.validate());
        Ok(problems)
    }

    fn check_build_script_output(&self, output: proxy::rpc::BuildScriptOutput) -> Problems {
        build_script_checker::check(&output, &self.config)
    }
}
