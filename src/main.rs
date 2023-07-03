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
pub(crate) mod fs;
pub(crate) mod link_info;
mod logging;
mod outcome;
pub(crate) mod problem;
pub(crate) mod problem_store;
mod proxy;
mod sandbox;
pub(crate) mod section_name;
mod summary;
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
use crate_index::CrateIndex;
use events::AppEvent;
use log::info;
use outcome::ExitCode;
use outcome::Outcome;
use problem::Problem;
use problem_store::ProblemStoreRef;
use proxy::rpc::Request;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use summary::SummaryOptions;
use symbol_graph::GraphOutputs;

#[derive(Parser, Debug, Clone, Default)]
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

#[derive(Subcommand, Debug, Clone, Default)]
enum Command {
    /// Non-interactive check of configuration.
    #[default]
    Check,
    /// Interactive check of configuration.
    Ui(UiArgs),
    /// Print summary of permissions used.
    Summary(SummaryOptions),
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
    info!("Shutdown with exit code {}", exit_code);
    std::process::exit(exit_code.code());
}

struct Cackle {
    problem_store: ProblemStoreRef,
    root_path: PathBuf,
    config_path: PathBuf,
    checker: Arc<Mutex<Checker>>,
    target_dir: PathBuf,
    args: Arc<Args>,
    event_sender: Sender<AppEvent>,
    ui_join_handle: JoinHandle<Result<()>>,
    crate_index: Arc<CrateIndex>,
}

impl Cackle {
    fn new(args: Args) -> Result<Self> {
        let args = Arc::new(args);
        let root_path = args
            .path
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .ok_or_else(|| anyhow!("Failed to get current working directory"))?;
        let root_path = Path::new(&root_path)
            .canonicalize()
            .with_context(|| format!("Failed to read directory `{}`", root_path.display()))?;

        proxy::clean(&root_path, &args)?;

        let config_path = args
            .cackle_path
            .clone()
            .unwrap_or_else(|| root_path.join("cackle.toml"));

        let crate_index = Arc::new(CrateIndex::new(&root_path)?);
        let target_dir = root_path.join("target");
        let mut checker = Checker::new(
            target_dir.clone(),
            args.clone(),
            crate_index.clone(),
            config_path.clone(),
        );
        for crate_name in &crate_index.proc_macros {
            checker.report_proc_macro(crate_name);
        }
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let problem_store = crate::problem_store::create(event_sender.clone());
        let ui_join_handle =
            ui::start_ui(&args, &config_path, problem_store.clone(), event_receiver)?;
        Ok(Self {
            problem_store,
            root_path,
            config_path,
            checker: Arc::new(Mutex::new(checker)),
            target_dir,
            args,
            event_sender,
            ui_join_handle,
            crate_index,
        })
    }

    /// Runs, reports any error and returns the exit code. Takes self by value so that it's dropped
    /// before we return. That way the user interface will be cleaned up before we exit.
    fn run_and_report_errors(mut self) -> ExitCode {
        if let Command::Summary(options) = &self.args.command {
            return self.print_summary(options);
        }
        let exit_code = match self.run() {
            Err(error) => {
                self.problem_store.report_error(error);
                outcome::FAILURE
            }
            Ok(exit_code) => exit_code,
        };
        let _ = self.event_sender.send(AppEvent::Shutdown);
        if let Ok(Err(error)) = self.ui_join_handle.join() {
            println!("UI error: {error}");
            return outcome::FAILURE;
        }
        if self.args.print_path_to_crate_map {
            self.checker.lock().unwrap().print_path_to_crate_map();
        }
        if exit_code == outcome::SUCCESS {
            let checker = self.checker.lock().unwrap();
            let summary = summary::Summary::new(&self.crate_index, &checker.config);
            println!("{summary}");
        }
        exit_code
    }

    fn print_summary(&self, options: &SummaryOptions) -> ExitCode {
        let mut checker = self.checker.lock().unwrap();
        if let Err(error) = checker.load_config() {
            println!("{error:#}");
            return outcome::FAILURE;
        }
        let summary = summary::Summary::new(&self.crate_index, &checker.config);
        summary.print(options);
        outcome::SUCCESS
    }

    fn run(&mut self) -> Result<ExitCode> {
        if self.maybe_create_config()? == Outcome::GiveUp {
            info!("Gave up creating initial configuration");
            return Ok(outcome::FAILURE);
        }
        self.checker.lock().unwrap().load_config()?;

        let mut initial_outcome = self.new_request_handler(None).handle_request()?;
        let config_path = crate::config::flattened_config_path(&self.target_dir);
        let config = self.checker.lock().unwrap().config.clone();
        let crate_index = self.checker.lock().unwrap().crate_index.clone();
        initial_outcome = initial_outcome.and(
            self.problem_store
                .fix_problems(config.unused_imports(&crate_index)),
        );

        {
            let mut checker = self.checker.lock().unwrap();

            // The following call to load_config is only really necessary if we fixed unused-import
            // problems above. It might be worthwhile at some point refactoring so that we don't do an
            // unnecessary reload here.
            checker.load_config()?;

            for crate_name in self.crate_index.crate_names() {
                checker.report_crate_used(crate_name);
            }
        }

        let root_path = self.root_path.clone();
        let args = self.args.clone();
        let build_result = if initial_outcome == Outcome::Continue {
            proxy::invoke_cargo_build(&root_path, &config_path, &config, &args, |request| {
                self.new_request_handler(Some(request))
            })
        } else {
            // We've already detected problems before running cargo, don't run cargo.
            Ok(None)
        };

        if self.problem_store.lock().has_aborted {
            return Ok(outcome::FAILURE);
        }

        // We only check if the build failed if there were no ACL check errors.
        if let Some(build_failure) = build_result? {
            println!("Build failure: {build_failure}");
            info!("Build failure: {build_failure}");
            return Ok(outcome::FAILURE);
        }

        let unused_problems = self.checker.lock().unwrap().check_unused();
        let resolution = self.problem_store.fix_problems(unused_problems);
        if resolution != Outcome::Continue {
            return Ok(outcome::FAILURE);
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

        Ok(outcome::SUCCESS)
    }

    fn new_request_handler(&self, request: Option<Request>) -> RequestHandler {
        RequestHandler {
            check_state: CheckState::default(),
            checker: self.checker.clone(),
            problem_store: self.problem_store.clone(),
            request,
        }
    }

    fn maybe_create_config(&mut self) -> Result<Outcome> {
        if !self.config_path.exists() {
            return Ok(self
                .problem_store
                .fix_problems(Problem::MissingConfiguration(self.config_path.clone()).into()));
        }
        Ok(Outcome::Continue)
    }
}

impl Args {
    fn ui_kind(&self) -> ui::Kind {
        match &self.command {
            Command::Check => ui::Kind::None,
            Command::Ui(ui_args) => ui_args.ui,
            Command::Summary(..) => ui::Kind::None,
        }
    }
}

#[derive(Default)]
struct CheckState {
    graph_outputs: Option<GraphOutputs>,
}

struct RequestHandler {
    check_state: CheckState,
    checker: Arc<Mutex<Checker>>,
    problem_store: ProblemStoreRef,
    request: Option<proxy::rpc::Request>,
}

impl RequestHandler {
    fn handle_request(&mut self) -> Result<Outcome> {
        loop {
            let problems = self
                .checker
                .lock()
                .unwrap()
                .problems(&self.request, &mut self.check_state)?;
            let return_on_retry = problems.should_send_retry_to_subprocess();
            if problems.is_empty() {
                return Ok(Outcome::Continue);
            }
            match self.problem_store.fix_problems(problems) {
                Outcome::Continue => {
                    self.checker.lock().unwrap().load_config()?;
                    if return_on_retry {
                        // If the only problem is that something in a subprocess failed, we return
                        // an empty error set. This signals the subprocess that it should proceed,
                        // which since something failed means that it should reload the config and
                        // retry whatever failed.
                        return Ok(Outcome::Continue);
                    }
                }
                Outcome::GiveUp => {
                    return Ok(Outcome::GiveUp);
                }
            }
        }
    }
}

const _CHECK_OS: () = if cfg!(all(
    not(target_os = "linux"),
    not(feature = "unsupported-os")
)) {
    panic!("Sorry, only Linux is currently supported. See PORTING.md");
};
