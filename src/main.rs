//! Analyses rust crates and their dependent crates to see what categories of APIs and language
//! features are used.

#![forbid(unsafe_code)]
#![cfg_attr(not(feature = "ui"), allow(dead_code, unused_variables))]

mod build_script_checker;
mod checker;
mod colour;
mod config;
mod config_editor;
mod config_validation;
mod cowarc;
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
mod tmpdir;
mod ui;
mod unsafe_checker;

use crate::proxy::subprocess::PROXY_BIN_ARG;
use anyhow::anyhow;
use anyhow::bail;
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
use proxy::cargo::profile_name;
use proxy::cargo::CargoOptions;
use proxy::rpc::Request;
use proxy::CargoOutputWaiter;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use summary::SummaryOptions;
use symbol_graph::ScanOutputs;
use tmpdir::TempDir;

#[derive(Parser, Debug, Clone)]
#[clap()]
struct OuterArgs {
    #[command(subcommand)]
    command: OuterCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum OuterCommand {
    Acl(Args),
}

#[derive(Parser, Debug, Clone, Default)]
#[clap(version, about)]
struct Args {
    /// Directory containing crate to analyze. Defaults to current working directory.
    #[clap(short, long)]
    path: Option<PathBuf>,

    /// Path to cackle.toml. Defaults to cackle.toml in the directory containing Cargo.toml.
    #[clap(short, long)]
    cackle_path: Option<PathBuf>,

    /// Print the mapping from paths to crate names. Useful for debugging.
    #[clap(long, hide = true)]
    print_path_to_crate_map: bool,

    /// Promotes warnings (e.g. due to unused permissions) to errors.
    #[clap(long)]
    fail_on_warnings: bool,

    /// Ignore newer config versions.
    #[clap(long)]
    ignore_newer_config_versions: bool,

    /// Whether to use coloured output.
    #[clap(long, alias = "color", default_value = "auto")]
    colour: colour::Colour,

    /// Don't print anything on success.
    #[clap(long)]
    quiet: bool,

    /// Override the target used when compiling. e.g. "x86_64-unknown-linux-gnu".
    #[clap(long)]
    target: Option<String>,

    /// Override build profile.
    #[clap(long)]
    profile: Option<String>,

    /// Features to pass to cargo. Overrides common.features in config.
    #[clap(long)]
    features: Option<String>,

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

    /// When specified, writes all requests into a subdirectory of the target directory. For
    /// debugging use.
    #[clap(long, hide = true)]
    save_requests: bool,

    /// Instead of running `cargo build`, replay requests saved by a previous run where
    /// --write-requests was specified. For debugging use.
    #[clap(long, hide = true)]
    replay_requests: bool,

    /// Temporary directory for Cackle to use. This is intended for testing purposes.
    #[clap(long, hide = true)]
    tmpdir: Option<PathBuf>,

    /// What kind of user interface to use.
    #[clap(long)]
    ui: Option<ui::Kind>,

    /// Disable interactive UI.
    #[clap(long, short)]
    no_ui: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug, Clone)]
enum Command {
    /// Print summary of permissions used.
    Summary(SummaryOptions),

    /// Run `cargo test`, analysing whatever gets built.
    Test(CargoOptions),

    /// Run `cargo run`, analysing whatever gets built.
    Run(CargoOptions),

    #[clap(hide = true, name = PROXY_BIN_ARG)]
    ProxyBin(ProxyBinOptions),
}

#[derive(Parser, Debug, Clone)]
pub(crate) struct ProxyBinOptions {
    #[clap(allow_hyphen_values = true)]
    remaining: Vec<String>,
}

fn main() -> Result<()> {
    proxy::subprocess::handle_wrapped_binaries()?;

    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == PROXY_BIN_ARG)
    {
        // If we get here and the call to handle_wrapped_binaries above didn't diverge, then either
        // a user invoked a bin wrapper directly, or we've been invoked when we're already inside a
        // cackle sandbox. In either case, we just run the original binary directly.
        return invoke_wrapped_binary();
    }

    let outer = OuterArgs::parse();
    let OuterCommand::Acl(mut args) = outer.command;
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
    tmpdir: Arc<TempDir>,
    target_dir: PathBuf,
    args: Arc<Args>,
    event_sender: Sender<AppEvent>,
    ui_join_handle: JoinHandle<Result<()>>,
    cargo_output_waiter: Option<CargoOutputWaiter>,
    crate_index: Arc<CrateIndex>,
    abort_sender: Sender<()>,
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

        let config_path = args
            .cackle_path
            .clone()
            .unwrap_or_else(|| root_path.join("cackle.toml"));

        let crate_index = Arc::new(CrateIndex::new(&root_path)?);
        let target_dir = root_path.join("target");
        let tmpdir = Arc::new(TempDir::new(args.tmpdir.as_deref())?);
        let checker = Arc::new(Mutex::new(Checker::new(
            tmpdir.clone(),
            target_dir.clone(),
            args.clone(),
            determine_sysroot(&root_path)?,
            crate_index.clone(),
            config_path.clone(),
        )));
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let problem_store = crate::problem_store::create(event_sender.clone());
        let ui_join_handle = ui::start_ui(
            &args,
            &config_path,
            &checker,
            problem_store.clone(),
            crate_index.clone(),
            event_receiver,
            abort_sender.clone(),
        )?;
        Ok(Self {
            problem_store,
            root_path,
            config_path,
            checker,
            args,
            event_sender,
            ui_join_handle,
            crate_index,
            tmpdir,
            target_dir,
            abort_sender,
            cargo_output_waiter: None,
        })
    }

    /// Runs, reports any error and returns the exit code. Takes self by value so that it's dropped
    /// before we return. That way the user interface will be cleaned up before we exit.
    fn run_and_report_errors(mut self, abort_recv: Receiver<()>) -> ExitCode {
        if let Some(Command::Summary(options)) = &self.args.command {
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
        if let Some(mut output_waiter) = self.cargo_output_waiter.take() {
            output_waiter.wait_for_output();
        }
        // Now that the UI (if any) has shut down, print any errors.
        if let Some(error) = error {
            println!();
            println!("Error: {error:#}");
        }

        let checker = self.checker.lock().unwrap();
        if self.args.print_path_to_crate_map {
            checker.print_path_to_crate_map();
        }
        if self.args.print_timing {
            checker.print_timing();
        }
        if exit_code == outcome::SUCCESS && !self.args.quiet && self.args.command.is_none() {
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
        {
            let should_run_cargo_clean = self.should_run_cargo_clean();
            let checker = &mut self.checker.lock().unwrap();
            checker.load_config()?;

            if should_run_cargo_clean {
                proxy::clean(&self.root_path, &self.args, &checker.config.raw.common)?;
            }
        }
        if !self.args.ignore_newer_config_versions {
            let update_problems = self.checker.lock().unwrap().check_for_new_config_version();
            if !update_problems.is_empty() {
                self.problem_store.fix_problems(update_problems);
            }
        }

        let mut initial_outcome = self.new_request_handler(None).handle_request()?;
        let config = self.checker.lock().unwrap().config.clone();
        let crate_index = self.checker.lock().unwrap().crate_index.clone();
        initial_outcome = initial_outcome.and(
            self.problem_store
                .fix_problems(config.raw.unused_imports(&crate_index)),
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
            if self.args.replay_requests {
                self.replay_requests()
            } else {
                let cargo_runner = proxy::CargoRunner {
                    manifest_dir: &root_path,
                    tmpdir: self.tmpdir.path(),
                    target_dir: &self.target_dir,
                    config: &config,
                    args: &args,
                    crate_index: &crate_index,
                };
                let r = cargo_runner.invoke_cargo_build(
                    abort_recv,
                    self.abort_sender.clone(),
                    |request| {
                        if self.args.save_requests {
                            if let Err(error) = self.save_request(&request) {
                                println!("Failed to save request: {error}");
                            }
                        }
                        self.new_request_handler(Some(request))
                    },
                );
                match r {
                    Ok(output_waiter) => {
                        self.cargo_output_waiter = Some(output_waiter);
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
        } else {
            // We've already detected problems before running cargo, don't run cargo.
            Ok(())
        };

        if self.problem_store.lock().has_aborted {
            return Ok(outcome::FAILURE);
        }

        // We only check if the build failed if there were no ACL check errors.
        build_result?;

        // If we didn't run `cargo clean` when we started, then our records of what is an isn't used
        // won't be complete, so we shouldn't emit unused warnings.
        if self.should_run_cargo_clean() {
            let unused_problems = self.checker.lock().unwrap().check_unused()?;
            let resolution = self.problem_store.fix_problems(unused_problems);
            if resolution != Outcome::Continue {
                return Ok(outcome::FAILURE);
            }
        }

        Ok(outcome::SUCCESS)
    }

    fn should_run_cargo_clean(&mut self) -> bool {
        !self.args.replay_requests && self.args.command.is_none()
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

    fn saved_request_path(&self) -> PathBuf {
        self.root_path
            .join("target")
            .join(profile_name(
                &self.args,
                &self.checker.lock().unwrap().config.raw.common,
            ))
            .join("saved-cackle-rpcs")
    }

    fn replay_requests(&self) -> Result<()> {
        let rpcs_dir = &self.saved_request_path();
        let mut rpc_paths: Vec<PathBuf> = rpcs_dir
            .read_dir()
            .with_context(|| format!("Failed to read saved RPCs dir `{}`", rpcs_dir.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect();
        rpc_paths.sort();
        for path in rpc_paths {
            info!("Replaying RPC `{}`", path.display());
            if self
                .replay_request(&path)
                .with_context(|| format!("Replay of request `{}` failed", path.display()))?
                == Outcome::GiveUp
            {
                bail!("Request gave error");
            }
        }
        Ok(())
    }

    fn replay_request(&self, path: &Path) -> Result<Outcome> {
        let request_str = crate::fs::read_to_string(path)?;
        let request: Request = serde_json::from_str(&request_str)?;
        let mut handler = self.new_request_handler(Some(request));
        handler.handle_request()
    }

    fn save_request(&self, request: &Request) -> Result<()> {
        let rpcs_dir = self.saved_request_path();
        std::fs::create_dir_all(&rpcs_dir)?;
        let num_entries = rpcs_dir.read_dir()?.count();
        let serialized = serde_json::to_string(request)?;
        std::fs::write(
            rpcs_dir.join(format!("{num_entries:03}.cackle-rpc")),
            serialized,
        )?;
        Ok(())
    }
}

fn determine_sysroot(root_path: &PathBuf) -> Result<Arc<Path>> {
    let output = std::process::Command::new("rustc")
        .current_dir(root_path)
        .arg("--print")
        .arg("sysroot")
        .output()
        .context("Failed to run `rustc --print sysroot`")?;
    let stdout = std::str::from_utf8(&output.stdout).context("rust sysroot isn't UTF-8")?;
    Ok(Arc::from(Path::new(stdout.trim())))
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
                .handle_request(&self.request, &mut self.check_state)?;
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

/// Directly invokes a wrapped binary, where the binary and arguments were passed to us by the
/// wrapper shell script.
fn invoke_wrapped_binary() -> Result<()> {
    let mut args = std::env::args_os().skip(3);
    let program = args
        .next()
        .ok_or_else(|| anyhow!("Missing proxy-bin program"))?;
    let status = std::process::Command::new(&program)
        .args(args)
        .status()
        .with_context(|| format!("Failed to invoke `{}`", program.to_string_lossy()))?;
    std::process::exit(status.code().unwrap_or(-1));
}

const _CHECK_OS: () = if cfg!(all(
    not(target_os = "linux"),
    not(feature = "unsupported-os")
)) {
    panic!("Sorry, only Linux is currently supported. See PORTING.md");
};
