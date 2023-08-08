//! Analyses rust crates and their dependent crates to see what categories of APIs and language
//! features are used.

#![forbid(unsafe_code)]
#![cfg_attr(not(feature = "ui"), allow(dead_code))]

mod build_script_checker;
mod bytes;
mod checker;
mod colour;
mod config;
#[cfg(feature = "ui")]
mod config_editor;
mod config_validation;
mod crate_index;
mod demangle;
mod deps;
pub(crate) mod events;
pub(crate) mod fs;
pub(crate) mod link_info;
pub(crate) mod location;
mod logging;
mod names;
mod outcome;
pub(crate) mod problem;
pub(crate) mod problem_store;
mod proxy;
mod sandbox;
mod summary;
pub(crate) mod symbol;
mod symbol_graph;
mod timing;
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
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use summary::SummaryOptions;
use symbol_graph::ScanOutputs;

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

    /// Build profile to use. This is currently for testing purposes and isn't yet properly
    /// supported. In particular, the selected profile needs to satisfy certain criteria and failure
    /// to meet those criteria leads to surprising behaviour.
    #[clap(long, default_value = proxy::cargo::DEFAULT_PROFILE_NAME, hide = true)]
    profile: String,

    /// Print how long various things take to run.
    #[clap(long)]
    print_timing: bool,

    /// Print additional information that's probably only useful for debugging.
    #[clap(long)]
    debug: bool,

    /// Output file for logs that might be useful for diagnosing problems.
    #[clap(long)]
    log_file: Option<PathBuf>,

    /// How detailed the logs should be.
    #[clap(long, default_value = "info")]
    log_level: logging::LevelFilter,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug, Clone, Default)]
enum Command {
    /// Non-interactive check of configuration.
    #[default]
    Check,
    /// Interactive check of configuration.
    #[cfg(feature = "ui")]
    Ui(ui::UiArgs),
    /// Print summary of permissions used.
    Summary(SummaryOptions),
}

fn main() -> Result<()> {
    proxy::subprocess::handle_wrapped_binaries()?;

    let mut args = Args::parse();
    args.colour = args.colour.detect();
    if let Some(log_file) = &args.log_file {
        logging::init(log_file, args.log_level)?;
    }
    let (abort_send, abort_recv) = std::sync::mpsc::channel();
    let cackle = Cackle::new(args, abort_send)?;
    let exit_code = cackle.run_and_report_errors(abort_recv);
    info!("Shutdown with exit code {}", exit_code);
    std::process::exit(exit_code.code());
}

struct Cackle {
    problem_store: ProblemStoreRef,
    root_path: PathBuf,
    config_path: PathBuf,
    checker: Arc<Mutex<Checker>>,
    tmpdir: Arc<tempfile::TempDir>,
    args: Arc<Args>,
    event_sender: Sender<AppEvent>,
    ui_join_handle: JoinHandle<Result<()>>,
    crate_index: Arc<CrateIndex>,
}

impl Cackle {
    fn new(args: Args, abort_sender: Sender<()>) -> Result<Self> {
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
        let tmpdir = Arc::new(tempfile::TempDir::new()?);
        let mut checker = Checker::new(
            tmpdir.clone(),
            args.clone(),
            crate_index.clone(),
            config_path.clone(),
        );
        for crate_name in crate_index.proc_macros() {
            checker.report_proc_macro(crate_name);
        }
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let problem_store = crate::problem_store::create(event_sender.clone());
        let ui_join_handle = ui::start_ui(
            &args,
            &config_path,
            problem_store.clone(),
            crate_index.clone(),
            event_receiver,
            abort_sender,
        )?;
        Ok(Self {
            problem_store,
            root_path,
            config_path,
            checker: Arc::new(Mutex::new(checker)),
            args,
            event_sender,
            ui_join_handle,
            crate_index,
            tmpdir,
        })
    }

    /// Runs, reports any error and returns the exit code. Takes self by value so that it's dropped
    /// before we return. That way the user interface will be cleaned up before we exit.
    fn run_and_report_errors(mut self, abort_recv: Receiver<()>) -> ExitCode {
        if let Command::Summary(options) = &self.args.command {
            return self.print_summary(options);
        }
        let mut error = None;
        let exit_code = match self.run(abort_recv) {
            Err(e) => {
                error = Some(e);
                outcome::FAILURE
            }
            Ok(exit_code) => exit_code,
        };
        let _ = self.event_sender.send(AppEvent::Shutdown);
        if let Ok(Err(error)) = self.ui_join_handle.join() {
            println!("UI error: {error}");
            return outcome::FAILURE;
        }
        // Now that the UI (if any) has shut down, print any errors.
        if let Some(error) = error {
            println!("{error:#}");
        }

        let checker = self.checker.lock().unwrap();
        if self.args.print_path_to_crate_map {
            checker.print_path_to_crate_map();
        }
        if self.args.print_timing {
            checker.print_timing();
        }
        if exit_code == outcome::SUCCESS && !self.args.quiet {
            println!(
                "Completed successfully for configuration {}",
                self.config_path.display()
            );
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

    fn run(&mut self, abort_recv: Receiver<()>) -> Result<ExitCode> {
        if self.maybe_create_config()? == Outcome::GiveUp {
            info!("Gave up creating initial configuration");
            return Ok(outcome::FAILURE);
        }
        self.checker.lock().unwrap().load_config()?;

        let mut initial_outcome = self.new_request_handler(None).handle_request()?;
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
        }

        let root_path = self.root_path.clone();
        let args = self.args.clone();
        let build_result = if initial_outcome == Outcome::Continue {
            proxy::invoke_cargo_build(
                &root_path,
                &self.tmpdir,
                &config,
                &args,
                abort_recv,
                &crate_index,
                |request| self.new_request_handler(Some(request)),
            )
        } else {
            // We've already detected problems before running cargo, don't run cargo.
            Ok(())
        };

        if self.problem_store.lock().has_aborted {
            return Ok(outcome::FAILURE);
        }

        // We only check if the build failed if there were no ACL check errors.
        build_result?;

        let unused_problems = self.checker.lock().unwrap().check_unused();
        let resolution = self.problem_store.fix_problems(unused_problems);
        if resolution != Outcome::Continue {
            return Ok(outcome::FAILURE);
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

#[derive(Default)]
struct CheckState {
    graph_outputs: Option<ScanOutputs>,
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
