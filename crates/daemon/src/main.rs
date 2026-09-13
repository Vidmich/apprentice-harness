//! `harnessd` — the apprentice-harness daemon.
//!
//! One process per user and data directory hosts the core (config, trace
//! store, mentor) and serves the RPC API on a local socket. Clients find it
//! through `daemon.json` and start it when it is not running; see task
//! M00-08. `--stdio` instead serves the one client on stdin/stdout.

mod lifecycle;
mod lock;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use apprentice_core::app::AppState;
use apprentice_core::config::{Config, ConfigLoader, Paths};
use apprentice_core::telemetry;
use clap::Parser;

use crate::lock::{DaemonLock, LockError};

const NAME: &str = "harnessd";

/// Exit codes.
mod exit {
    pub const OK: u8 = 0;
    pub const FAILED: u8 = 1;
    /// Another daemon holds the lock for this data directory.
    pub const ALREADY_RUNNING: u8 = 3;
}

#[derive(Debug, Parser)]
#[command(name = NAME, version = apprentice_core::VERSION, about = "apprentice-harness daemon")]
struct Args {
    /// Stay attached to the terminal and log to stderr.
    #[arg(long)]
    foreground: bool,

    /// Serve one connection over stdin/stdout instead of the local socket.
    #[arg(long)]
    stdio: bool,

    /// Put config and data under this directory (same as `HARNESS_HOME`).
    #[arg(long, value_name = "DIR", env = "HARNESS_HOME")]
    home: Option<PathBuf>,

    /// Log filter, e.g. `debug` or `apprentice_core=trace,info`.
    #[arg(long, value_name = "LEVEL")]
    log_level: Option<String>,

    /// Exit after this many seconds without clients (overrides
    /// `daemon.idle_shutdown_min`; 0 = never). For tests and development.
    #[arg(long, value_name = "SECONDS", hide = true)]
    idle_secs: Option<u64>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            // Logging may not be up; stderr is the one place guaranteed.
            eprintln!("{NAME}: error: {e:#}");
            tracing::error!(error = format!("{e:#}"), "fatal");
            ExitCode::from(exit::FAILED)
        }
    }
}

fn run(args: &Args) -> anyhow::Result<ExitCode> {
    let paths = match &args.home {
        Some(home) => Paths::from_home(home),
        None => Paths::discover()?,
    };
    std::fs::create_dir_all(&paths.data_dir)
        .with_context(|| format!("cannot create data dir {}", paths.data_dir.display()))?;

    // Config may be broken; logging must still come up so the error lands
    // in the log, and the daemon still starts on defaults so the file can
    // be fixed through `config.set`.
    let loader = ConfigLoader::new(paths.clone()).with_process_env()?;
    let config = loader.load(None);
    let config_level = config
        .as_ref()
        .ok()
        .map(|r| r.config.daemon.log_level.clone());
    let level = telemetry::resolve_level(
        args.log_level.as_deref(),
        telemetry::level_from_env().as_deref(),
        config_level.as_deref(),
    );
    let log = telemetry::init(
        &telemetry::Options::new("daemon", level, paths.data_dir.join("logs"))
            .stderr(args.foreground || args.stdio),
    )?;
    telemetry::install_panic_hook_with(|| {
        format!(
            "last rpc method: {}",
            apprentice_api::server::last_method().unwrap_or_else(|| "none".to_owned())
        )
    });

    tracing::info!(
        version = apprentice_core::VERSION,
        api_version = apprentice_api::API_VERSION,
        pid = std::process::id(),
        data_dir = %paths.data_dir.display(),
        log_file = %log.log_file.display(),
        filter = log.filter,
        stdio = args.stdio,
        "starting"
    );
    let config = match config {
        Ok(r) => r.config,
        Err(e) => {
            tracing::error!(error = %e, "configuration is invalid; using defaults");
            Config::default()
        }
    };

    // The lock comes before the store so two starters never race on it.
    let lock = if args.stdio {
        None
    } else {
        match DaemonLock::acquire(&paths.data_dir) {
            Ok(lock) => {
                tracing::debug!(lock = %lock.path().display(), "lock acquired");
                Some(lock)
            }
            Err(e @ LockError::Held { .. }) => {
                eprintln!("{NAME}: {e}");
                tracing::warn!(error = %e, "not starting");
                return Ok(ExitCode::from(exit::ALREADY_RUNNING));
            }
            Err(e) => return Err(e.into()),
        }
    };

    let state = AppState::open_with(loader, &config)?;
    let idle = match args.idle_secs {
        Some(secs) => Duration::from_secs(secs),
        None => Duration::from_secs(config.daemon.idle_shutdown_min * 60),
    };
    let options = lifecycle::Options {
        idle: (!idle.is_zero()).then_some(idle),
        log_file: Some(log.log_file.clone()),
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("harnessd")
        .build()
        .context("cannot start async runtime")?;
    let result = runtime.block_on(async {
        if args.stdio {
            lifecycle::serve_stdio(state, options).await
        } else {
            lifecycle::serve_socket(state, options).await
        }
    });
    // Blocking stdin reads (stdio mode) must not hold the process open.
    runtime.shutdown_timeout(Duration::from_secs(1));
    drop(lock);
    result.map(|()| ExitCode::from(exit::OK))
}

#[cfg(test)]
mod tests {
    #[test]
    fn name_is_stable() {
        assert_eq!(super::NAME, "harnessd");
    }

    #[test]
    fn args_parse() {
        use clap::Parser;
        let a = super::Args::try_parse_from([
            "harnessd",
            "--foreground",
            "--log-level",
            "trace",
            "--idle-secs",
            "5",
        ])
        .unwrap();
        assert!(a.foreground && !a.stdio);
        assert_eq!(a.log_level.as_deref(), Some("trace"));
        assert_eq!(a.idle_secs, Some(5));
    }
}
