//! `chargecapd`: the root daemon that holds the battery inside a charge band.
//!
//! Sub-commands:
//!
//! - `daemon` runs the control loop and the socket server in the foreground.
//! - `install` and `uninstall` manage the LaunchDaemon. Both need root.
//! - `status` and `limit` are clients of a running daemon.

mod client;
mod config;
mod control;
mod install;
mod logging;
mod server;
mod state;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use config::Config;
use control::LOOP_INTERVAL;
use state::Daemon;

/// How often the daemon checks for a shutdown signal while it waits.
const SHUTDOWN_POLL: Duration = Duration::from_millis(200);

/// Set by the SIGTERM and SIGINT handlers.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

#[derive(Parser)]
#[command(
    name = "chargecapd",
    version,
    about = "chargecap battery limiter daemon"
)]
struct Cli {
    /// Print the raw JSON reply for `status` and `limit`.
    #[arg(long, global = true)]
    json: bool,
    /// Use this socket path instead of the default.
    #[arg(long, global = true, value_name = "PATH")]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon in the foreground.
    Daemon {
        /// Run without root. Writes to the SMC will fail and be logged.
        #[arg(long)]
        allow_unprivileged: bool,
    },
    /// Install the LaunchDaemon.
    Install,
    /// Uninstall the LaunchDaemon.
    Uninstall,
    /// Print the current charge status.
    Status,
    /// Set the upper charge limit.
    Limit {
        /// Percent to stop charging at. 100 turns the limit off.
        upper: u8,
        /// Percent to restart charging at. Defaults to `upper` minus 2.
        lower: Option<u8>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match &cli.command {
        Command::Daemon { allow_unprivileged } => run_daemon(&cli, *allow_unprivileged),
        Command::Install => install::install(),
        Command::Uninstall => install::uninstall(&socket_for_daemon(&cli, install::is_root())),
        Command::Status => client::run(&socket_for_client(&cli), &proto::Request::Status, cli.json),
        Command::Limit { upper, lower } => client::run(
            &socket_for_client(&cli),
            &proto::Request::SetLimit {
                upper: *upper,
                lower: *lower,
            },
            cli.json,
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("chargecapd: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Returns the socket a daemon should bind.
fn socket_for_daemon(cli: &Cli, privileged: bool) -> PathBuf {
    match (&cli.socket, privileged) {
        (Some(path), _) => path.clone(),
        (None, true) => server::default_socket_path(),
        (None, false) => server::dev_socket_path(),
    }
}

/// Returns the socket a client should connect to.
fn socket_for_client(cli: &Cli) -> PathBuf {
    cli.socket
        .clone()
        .unwrap_or_else(server::client_socket_path)
}

/// Runs the control loop and the socket server until a signal arrives.
fn run_daemon(cli: &Cli, allow_unprivileged: bool) -> Result<()> {
    logging::init(std::path::Path::new(proto::LOG_PATH));
    let privileged = install::is_root();
    if !privileged && !allow_unprivileged {
        anyhow::bail!("the daemon needs root; run it with sudo or pass --allow-unprivileged");
    }
    install_signal_handlers();

    let driver = smc::IoKitDriver::open().context("cannot open the SMC")?;
    let smc = smc::Smc::new(driver);
    let mode = smc.charge_control_mode();

    let config_path = PathBuf::from(proto::CONFIG_PATH);
    let config = match Config::load(&config_path) {
        Ok(config) => config,
        Err(err) => {
            logging::error(format!(
                "cannot read {}: {err}; using defaults",
                config_path.display()
            ));
            Config::default()
        }
    };
    logging::info(format!(
        "chargecapd {} starting: mode {mode:?}, band {}/{}, root {privileged}",
        proto::VERSION,
        config.upper,
        config.lower
    ));

    let socket_path = socket_for_daemon(cli, privileged);
    let listener = server::bind(&socket_path)
        .with_context(|| format!("cannot bind {}", socket_path.display()))?;
    listener.set_nonblocking(true)?;
    logging::info(format!("listening on {}", socket_path.display()));

    let daemon = Mutex::new(Daemon::new(smc, config, &config_path));
    let serving = AtomicBool::new(true);

    std::thread::scope(|scope| {
        scope.spawn(|| server::serve(&listener, &daemon, &serving));

        while !SHUTDOWN.load(Ordering::SeqCst) {
            daemon.lock().unwrap_or_else(|err| err.into_inner()).tick();
            wait_for_next_tick();
        }
        serving.store(false, Ordering::SeqCst);
    });

    logging::info("shutting down");
    // Fail-safe: never leave the machine with the charge gate closed.
    daemon
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .reset_charge_control();
    if let Err(err) = std::fs::remove_file(&socket_path) {
        logging::warn(format!("cannot remove {}: {err}", socket_path.display()));
    }
    Ok(())
}

/// Sleeps one loop interval, in slices, so a signal is noticed at once.
fn wait_for_next_tick() {
    let deadline = Instant::now() + LOOP_INTERVAL;
    while Instant::now() < deadline && !SHUTDOWN.load(Ordering::SeqCst) {
        std::thread::sleep(SHUTDOWN_POLL);
    }
}

/// Asks for a clean shutdown on SIGTERM and SIGINT.
extern "C" fn on_signal(_signal: libc::c_int) {
    // Only an atomic store: this runs in a signal handler.
    SHUTDOWN.store(true, Ordering::SeqCst);
}

fn install_signal_handlers() {
    for signal in [libc::SIGTERM, libc::SIGINT] {
        // SAFETY: the handler is an `extern "C"` function that only does an
        // atomic store, which is safe to call from a signal handler.
        unsafe { libc::signal(signal, on_signal as *const () as libc::sighandler_t) };
    }
}
