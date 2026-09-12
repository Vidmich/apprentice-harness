//! `harness` — the apprentice-harness CLI.
//!
//! M00-04: global flags, logging and `doctor`. The rest of the command tree
//! arrives with M00-09. This binary must never depend on `apprentice-core`.

mod doctor;

use std::path::PathBuf;

use apprentice_common::paths::Paths;
use apprentice_common::telemetry;
use clap::{Parser, Subcommand};

const NAME: &str = "harness";

/// Exit codes (task M00-09).
mod exit {
    pub const OK: i32 = 0;
    pub const COMMAND_FAILED: i32 = 2;
}

#[derive(Debug, Parser)]
#[command(name = NAME, version = apprentice_client::VERSION, about = "apprentice-harness CLI")]
struct Cli {
    /// Put config and data under this directory (same as `HARNESS_HOME`).
    #[arg(long, global = true, value_name = "DIR", env = "HARNESS_HOME")]
    home: Option<PathBuf>,

    /// Print machine-readable JSON instead of text.
    #[arg(long, global = true)]
    json: bool,

    /// Suppress informational output on stderr.
    #[arg(long, global = true)]
    quiet: bool,

    /// Log filter, e.g. `debug` or `apprentice_client=trace,info`.
    #[arg(long, global = true, value_name = "LEVEL")]
    log_level: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print environment, paths, versions, daemon and hardware facts for bug reports.
    Doctor,
}

fn main() {
    let cli = Cli::parse();
    let code = match run(&cli) {
        Ok(()) => exit::OK,
        Err(e) => {
            eprintln!("error: {e:#}");
            exit::COMMAND_FAILED
        }
    };
    std::process::exit(code);
}

fn run(cli: &Cli) -> anyhow::Result<()> {
    let paths = match &cli.home {
        Some(home) => Paths::from_home(home),
        None => Paths::discover()?,
    };
    // The CLI cannot read config (that needs the core); flag > env > info.
    let level = telemetry::resolve_level(
        cli.log_level.as_deref(),
        telemetry::level_from_env().as_deref(),
        None,
    );
    // Logging is best effort for the CLI: an unwritable data dir must not
    // stop `doctor` from reporting exactly that.
    let log = telemetry::init(
        &telemetry::Options::new("cli", level, paths.data_dir.join("logs")).stderr(!cli.quiet),
    );
    if let Err(e) = &log {
        if !cli.quiet {
            eprintln!("warning: logging disabled: {e}");
        }
    }

    match &cli.command {
        Command::Doctor => doctor::run(&paths, log.ok().as_ref(), cli.json),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    #[test]
    fn name_is_stable() {
        assert_eq!(super::NAME, "harness");
    }

    #[test]
    fn global_flags_parse_after_the_subcommand() {
        let c = super::Cli::try_parse_from(["harness", "doctor", "--json", "--log-level", "debug"])
            .unwrap();
        assert!(c.json);
        assert_eq!(c.log_level.as_deref(), Some("debug"));
        assert!(matches!(c.command, super::Command::Doctor));
    }
}
