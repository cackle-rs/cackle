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
mod deps;
pub(crate) mod link_info;
pub(crate) mod problem;
mod proxy;
mod sandbox;
pub(crate) mod section_name;
pub(crate) mod symbol;
mod symbol_graph;
mod ui;
mod unsafe_checker;

use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use checker::Checker;
use clap::Parser;
use colored::Colorize;
use config::Config;
use crate_index::CrateIndex;
use link_info::LinkInfo;
use problem::Problems;
use std::path::Path;
use std::path::PathBuf;
use symbol_graph::SymGraph;

#[derive(Parser, Debug, Clone)]
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

    /// If set, warnings (e.g. due to unused permissions) will cause termination with a non-zero
    /// exit value.
    #[clap(long)]
    fail_on_warnings: bool,

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

    /// Don't print anything on success.
    #[clap(long)]
    quiet: bool,

    /// Override the target used when compiling. e.g. specify "x86_64-apple-darwin" to compile for
    /// x86 Mac. Note that build scripts and procedural macros will still be compiled for the host
    /// target.
    #[clap(long)]
    target: Option<String>,

    /// Provide additional information on some kinds of errors.
    #[clap(long)]
    verbose_errors: bool,
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

    if args.object_paths.is_empty() {
        proxy::clean(&root_path, &args)?;
    }

    let config_path = args
        .cackle_path
        .clone()
        .unwrap_or_else(|| root_path.join("cackle.toml"));

    let mut cackle = Cackle::new(config_path, &root_path, args.clone())?;
    cackle.maybe_create_config()?;
    cackle.load_config()?;

    if !cackle.args.object_paths.is_empty() {
        let paths: Vec<_> = cackle.args.object_paths.clone();
        let mut check_state = CheckState::default();
        report_problems_and_maybe_exit(&cackle.check_object_paths(&paths, &mut check_state)?);
        return Ok(());
    }

    let mut problems = cackle.unfixed_problems(None)?;
    let config_path = cackle.flattened_config_path();
    let config = cackle.config.clone();
    let build_result = if problems.is_empty() {
        proxy::invoke_cargo_build(&root_path, &config_path, &config, &args, |request| {
            problems.merge(cackle.unfixed_problems(Some(request))?);
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

    if let Err(unused) = cackle.checker.check_unused() {
        println!("{}", unused);
        if cackle.args.fail_on_warnings {
            println!(
                "{}: Warnings promoted to errors by --fail-on-warnings",
                "ERROR".red()
            );
            std::process::exit(-1);
        }
    }

    if !cackle.args.quiet {
        println!("Cackle succcess");
    }

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
    ui: Box<dyn ui::Ui>,
}

impl Cackle {
    fn new(config_path: PathBuf, root_path: &Path, args: Args) -> Result<Self> {
        let crate_index = CrateIndex::new(root_path)?;
        let mut checker = Checker::default();
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
        let ui: Box<dyn ui::Ui> = if args.non_interactive {
            Box::new(ui::NullUi)
        } else {
            Box::new(ui::BasicTermUi::new(config_path.clone()))
        };
        Ok(Self {
            config_path,
            config: Config::default(),
            checker,
            target_dir: root_path.join("target"),
            crate_index,
            args,
            ui,
        })
    }

    fn load_config(&mut self) -> Result<()> {
        let config = config::parse_file(&self.config_path, &self.crate_index)?;
        self.checker.load_config(&config);
        // Every time we reload our configuration, we rewrite the flattened configuration. The
        // flattened configuration is used by subprocesses rather than using the original
        // configuration since using the original would require each subprocess to run `cargo
        // metadata`.
        let flattened_path = self.flattened_config_path();
        if let Some(dir) = flattened_path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create directory `{}`", dir.display()))?;
        }
        std::fs::write(&flattened_path, config.flattened_toml()?)?;
        self.config = config;
        Ok(())
    }

    fn unfixed_problems(&mut self, request: Option<proxy::rpc::Request>) -> Result<Problems> {
        let mut check_state = CheckState::default();
        loop {
            let mut problems = self.problems(&request, &mut check_state)?;
            problems.condense();
            if problems.is_empty() {
                return Ok(problems);
            }
            match self.ui.maybe_fix_problems(&problems)? {
                ui::FixOutcome::Retry => {
                    self.load_config()?;
                    if problems.should_send_retry_to_subprocess() {
                        // If the only problem is that something in a subprocess failed, we return
                        // an empty error set. This signals the subprocess that it should proceed,
                        // which since something failed means that it should reload the config and
                        // retry whatever failed.
                        return Ok(Problems::default());
                    }
                }
                ui::FixOutcome::GiveUp => {
                    return Ok(problems);
                }
            }
        }
    }

    fn problems(
        &mut self,
        request: &Option<proxy::rpc::Request>,
        check_state: &mut CheckState,
    ) -> Result<Problems> {
        let Some(request) = request else {
            return Ok(self.checker.problems());
        };
        match request {
            proxy::rpc::Request::CrateUsesUnsafe(usage) => {
                Ok(self.checker.crate_uses_unsafe(usage))
            }
            proxy::rpc::Request::LinkerInvoked(link_info) => {
                self.check_linker_invocation(link_info, check_state)
            }
            proxy::rpc::Request::BuildScriptComplete(output) => {
                Ok(self.check_build_script_output(output))
            }
        }
    }

    fn check_linker_invocation(
        &mut self,
        info: &LinkInfo,
        check_state: &mut CheckState,
    ) -> Result<Problems> {
        let mut problems = Problems::default();
        if info.is_build_script {
            problems.merge(
                self.checker
                    .verify_build_script_permitted(&info.package_name),
            );
        }
        problems.merge(
            self.check_object_paths(&info.object_paths_under(&self.target_dir), check_state)?,
        );
        Ok(problems)
    }

    fn check_object_paths(
        &mut self,
        paths: &[PathBuf],
        check_state: &mut CheckState,
    ) -> Result<Problems> {
        if check_state.graph.is_none() {
            let mut graph = SymGraph::default();
            for path in paths {
                graph
                    .process_file(path)
                    .with_context(|| format!("Failed to process `{}`", path.display()))?;
            }
            check_state.graph = Some(graph);
        }
        let graph = check_state.graph.as_mut().unwrap();
        if self.args.print_all_references {
            println!("{graph}");
        }
        if self.config.needs_reachability() {
            let result = graph.compute_reachability(&self.args);
            if result.is_err() && self.args.verbose_errors {
                println!("Object paths:");
                for p in paths {
                    println!("  {}", p.display());
                }
            }
            result?;
        }
        let problems = graph.problems(&mut self.checker, &self.crate_index)?;
        Ok(problems)
    }

    fn check_build_script_output(&self, output: &proxy::rpc::BuildScriptOutput) -> Problems {
        build_script_checker::check(output, &self.config)
    }

    fn flattened_config_path(&self) -> PathBuf {
        self.target_dir
            .join(proxy::cargo::PROFILE_NAME)
            .join("flattened_cackle.toml")
    }

    fn maybe_create_config(&mut self) -> Result<()> {
        if !self.config_path.exists() {
            self.ui.create_initial_config()?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct CheckState {
    graph: Option<SymGraph>,
}

const _CHECK_OS: () = if cfg!(all(
    not(target_os = "linux"),
    not(feature = "unsupported-os")
)) {
    panic!("Sorry, only Linux is currently supported. See PORTING.md");
};
