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
pub(crate) mod events;
pub(crate) mod link_info;
mod logging;
pub(crate) mod problem;
pub(crate) mod problem_store;
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
use clap::Subcommand;
use colored::Colorize;
use config::Config;
use crate_index::CrateIndex;
use events::AppEvent;
use link_info::LinkInfo;
use problem::Problem;
use problem::ProblemList;
use problem_store::ProblemStoreRef;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use symbol_graph::SymGraph;
use ui::FixOutcome;

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

    /// Print how long various things take to run.
    #[clap(long)]
    print_timing: bool,

    /// Print additional information that's probably only useful for debugging.
    #[clap(long)]
    debug: bool,

    /// Output file for logs that might be useful for diagnosing problems.
    #[clap(long)]
    log_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug, Clone)]
enum Command {
    /// Non-interactive check of configuration.
    Check,
    /// Interactive check of configuration.
    Ui(UiArgs),
}

#[derive(Parser, Debug, Clone)]
struct UiArgs {
    /// What kind of user interface to use.
    #[clap(long, default_value = "full")]
    ui: ui::Kind,
}

fn main() -> Result<()> {
    proxy::subprocess::handle_wrapped_binaries()?;

    let mut args = Args::parse();
    args.colour = args.colour.detect();
    if let Some(log_file) = &args.log_file {
        logging::init(log_file)?;
    }
    let cackle = Cackle::new(args)?;
    let exit_code = cackle.run_and_report_errors();
    std::process::exit(exit_code);
}

struct Cackle {
    problem_store: ProblemStoreRef,
    root_path: PathBuf,
    config_path: PathBuf,
    config: Config,
    checker: Checker,
    target_dir: PathBuf,
    crate_index: CrateIndex,
    args: Args,
    event_sender: Sender<AppEvent>,
    ui_join_handle: JoinHandle<Result<()>>,
}

impl Cackle {
    fn new(args: Args) -> Result<Self> {
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

        let crate_index = CrateIndex::new(&root_path)?;
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
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let problem_store = crate::problem_store::create(event_sender.clone());
        let ui_join_handle = ui::start_ui(
            args.ui_kind(),
            &config_path,
            problem_store.clone(),
            event_receiver,
        )?;
        let target_dir = root_path.join("target");
        Ok(Self {
            problem_store,
            root_path,
            config_path,
            config: Config::default(),
            checker,
            target_dir,
            crate_index,
            args,
            event_sender,
            ui_join_handle,
        })
    }

    /// Runs, reports any error and returns the exit code. Takes self by value so that it's dropped
    /// before we return. That way the user interface will be cleaned up before we exit.
    fn run_and_report_errors(mut self) -> i32 {
        let exit_code = match self.run() {
            Err(error) => {
                self.problem_store.report_error(error);
                -1
            }
            Ok(exit_code) => exit_code,
        };
        let _ = self.event_sender.send(AppEvent::Shutdown);
        if let Ok(Err(error)) = self.ui_join_handle.join() {
            println!("{error}");
            return -1;
        }
        exit_code
    }

    fn run(&mut self) -> Result<i32> {
        if self.maybe_create_config()? == FixOutcome::GiveUp {
            return Ok(-1);
        }
        self.load_config()?;

        if !self.args.object_paths.is_empty() {
            let paths: Vec<_> = self.args.object_paths.clone();
            let mut check_state = CheckState::default();
            let problems = self.check_object_paths(&paths, &mut check_state)?;
            if !problems.is_empty() {
                for problem in &problems {
                    println!("{problem}");
                }
                return Ok(-1);
            }
            return Ok(0);
        }

        let initial_outcome = self.outcome_for_request(None)?;
        let config_path = self.flattened_config_path();
        let config = self.config.clone();
        let root_path = self.root_path.clone();
        let args = self.args.clone();
        let build_result =
            if initial_outcome == FixOutcome::Continue {
                proxy::invoke_cargo_build(&root_path, &config_path, &config, &args, |request| {
                    match self.outcome_for_request(Some(request))? {
                        FixOutcome::Continue => Ok(proxy::rpc::CanContinueResponse::Proceed),
                        FixOutcome::GiveUp => Ok(proxy::rpc::CanContinueResponse::Deny),
                    }
                })
            } else {
                // We've already detected problems before running cargo, don't run cargo.
                Ok(None)
            };

        // TODO: Should the NullUi be responsible for reporting errors in the non-interactive case?
        if !self.problem_store.lock().is_empty() {
            self.report_problems();
            return Ok(-1);
        }

        // We only check if the build failed if there were no ACL check errors.
        if let Some(build_failure) = build_result? {
            println!("{build_failure}");
            return Ok(-1);
        }

        let unused_problems = self.checker.check_unused();
        let resolution = self.problem_store.fix_problems(unused_problems);
        if resolution != FixOutcome::Continue {
            self.report_problems();
            return Ok(-1);
        }

        if !self.args.quiet {
            // TODO: Figure out how we want to report success.

            // self.ui.display_message(
            //     "Cackle success",
            //     &format!(
            //         "Completed successfully for configuration {}",
            //         self.config_path.display()
            //     ),
            // )?;
        }

        Ok(0)
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

    fn outcome_for_request(&mut self, request: Option<proxy::rpc::Request>) -> Result<FixOutcome> {
        let mut check_state = CheckState::default();
        loop {
            let problems = self.problems(&request, &mut check_state)?;
            let return_on_retry = problems.should_send_retry_to_subprocess();
            match self.problem_store.fix_problems(problems) {
                ui::FixOutcome::Continue => {
                    self.load_config()?;
                    if return_on_retry {
                        // If the only problem is that something in a subprocess failed, we return
                        // an empty error set. This signals the subprocess that it should proceed,
                        // which since something failed means that it should reload the config and
                        // retry whatever failed.
                        return Ok(FixOutcome::Continue);
                    }
                }
                ui::FixOutcome::GiveUp => {
                    return Ok(FixOutcome::GiveUp);
                }
            }
        }
    }

    fn report_problems(&self) {
        let mut pstore = self.problem_store.lock();
        pstore.group_by_crate();
        for (_, problem) in pstore.into_iter() {
            println!("{} {problem}", "ERROR:".red());
        }
    }

    fn problems(
        &mut self,
        request: &Option<proxy::rpc::Request>,
        check_state: &mut CheckState,
    ) -> Result<ProblemList> {
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
    ) -> Result<ProblemList> {
        let mut problems = ProblemList::default();
        if info.is_build_script {
            problems.merge(
                self.checker
                    .verify_build_script_permitted(&info.package_name),
            );
        }
        problems.merge(
            self.check_object_paths(&info.object_paths_under(&self.target_dir), check_state)?,
        );
        Ok(problems.grouped_by_type_crate_and_api())
    }

    fn check_object_paths(
        &mut self,
        paths: &[PathBuf],
        check_state: &mut CheckState,
    ) -> Result<ProblemList> {
        if self.args.debug {
            println!(
                "{}",
                paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        if check_state.graph.is_none() {
            let start = std::time::Instant::now();
            let mut graph = SymGraph::default();
            for path in paths {
                graph
                    .process_file(path)
                    .with_context(|| format!("Failed to process `{}`", path.display()))?;
            }
            if self.args.print_timing {
                println!("Graph computation took {}ms", start.elapsed().as_millis());
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
        let start = std::time::Instant::now();
        let problems = graph.problems(&mut self.checker, &self.crate_index)?;
        if self.args.print_timing {
            println!("API usage checking took {}ms", start.elapsed().as_millis());
        }
        Ok(problems)
    }

    fn check_build_script_output(&self, output: &proxy::rpc::BuildScriptOutput) -> ProblemList {
        build_script_checker::check(output, &self.config)
    }

    fn flattened_config_path(&self) -> PathBuf {
        self.target_dir
            .join(proxy::cargo::PROFILE_NAME)
            .join("flattened_cackle.toml")
    }

    fn maybe_create_config(&mut self) -> Result<FixOutcome> {
        if !self.config_path.exists() {
            return Ok(self
                .problem_store
                .fix_problems(Problem::MissingConfiguration(self.config_path.clone()).into()));
        }
        Ok(FixOutcome::Continue)
    }
}

impl Args {
    fn ui_kind(&self) -> ui::Kind {
        match &self.command {
            Command::Check => ui::Kind::None,
            Command::Ui(ui_args) => ui_args.ui,
        }
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
