//! `harnessd` — the apprentice-harness daemon.
//!
//! M00-04: flags, paths and logging are wired. Lifecycle, RPC serving and
//! core hosting arrive with M00-08; until then the process logs its startup
//! and exits.

use std::path::PathBuf;

use anyhow::Context;
use apprentice_core::config::{ConfigLoader, Paths};
use apprentice_core::telemetry;
use clap::Parser;

const NAME: &str = "harnessd";

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
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let paths = match &args.home {
        Some(home) => Paths::from_home(home),
        None => Paths::discover()?,
    };
    std::fs::create_dir_all(&paths.data_dir)
        .with_context(|| format!("cannot create data dir {}", paths.data_dir.display()))?;

    // Config may be broken; logging must still come up so the error lands
    // in the log. Fall back to defaults for the level in that case.
    let config = ConfigLoader::new(paths.clone())
        .with_process_env()?
        .load(None);
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

    tracing::info!(
        version = apprentice_core::VERSION,
        api_version = apprentice_api::API_VERSION,
        pid = std::process::id(),
        data_dir = %paths.data_dir.display(),
        log_file = %log.log_file.display(),
        filter = log.filter,
        "starting"
    );
    if let Err(e) = &config {
        tracing::error!(error = %e, "configuration is invalid; using defaults");
    }

    tracing::warn!("lifecycle not implemented yet (task M00-08); exiting");
    Ok(())
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
        let a = super::Args::try_parse_from(["harnessd", "--foreground", "--log-level", "trace"])
            .unwrap();
        assert!(a.foreground && !a.stdio);
        assert_eq!(a.log_level.as_deref(), Some("trace"));
    }
}
