//! `harness` — the apprentice-harness CLI (task M00-09).
//!
//! A thin client: every command talks to `harnessd` over the local socket
//! and this binary must never depend on `apprentice-core`. Global flags
//! come first or last (`harness --json doctor` and `harness doctor --json`
//! both work); `--json` prints exactly one JSON document per command
//! (NDJSON for `run`) with human messages on stderr.

mod auth;
mod config;
mod daemon;
mod doctor;
mod out;
mod prompt;
mod run;
mod session;
mod stats;
mod tools;
mod trace;
mod workspace;

use std::path::PathBuf;
use std::time::Duration;

use apprentice_client::{ClientError, ClientOptions, ConnectError};
use apprentice_common::paths::Paths;
use apprentice_common::telemetry;
use clap::{CommandFactory, Parser, Subcommand};

use crate::out::Out;

const NAME: &str = "harness";

/// Exit codes.
pub mod exit {
    pub const OK: i32 = 0;
    /// Bad arguments.
    pub const USAGE: i32 = 1;
    /// The command ran and failed.
    pub const COMMAND_FAILED: i32 = 2;
    /// No daemon could be reached or started.
    pub const DAEMON_UNAVAILABLE: i32 = 3;
    /// The daemon refused us: wrong token or incompatible API.
    pub const UNAUTHORIZED: i32 = 4;
    /// CTRL-C.
    pub const INTERRUPTED: i32 = 130;
}

/// Errors that map to an exit code other than "command failed".
#[derive(Debug, thiserror::Error)]
pub enum Exit {
    #[error("interrupted")]
    Interrupted,
    #[error("{0}")]
    Usage(String),
}

const AFTER_HELP: &str = "\
Exit codes:
  0  ok                     3  no daemon could be reached or started
  1  bad arguments          4  daemon refused us (token, API version)
  2  command failed       130  interrupted

Examples:
  harness run \"explain the failing test\"      # new session in the current directory
  harness session list --limit 5
  harness config set mentor.model claude-opus-5
  harness --json trace list --kind agent.finished";

#[derive(Debug, Parser)]
#[command(
    name = NAME,
    bin_name = NAME,
    version = apprentice_client::VERSION,
    about = "apprentice-harness CLI: run and inspect mentor/apprentice sessions",
    after_help = AFTER_HELP,
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    /// Put config and data under this directory (same as `HARNESS_HOME`).
    #[arg(long, global = true, value_name = "DIR", env = "HARNESS_HOME")]
    home: Option<PathBuf>,

    /// Print machine-readable JSON on stdout (NDJSON for `run`).
    #[arg(long, global = true)]
    json: bool,

    /// Suppress informational output on stderr.
    #[arg(long, global = true)]
    quiet: bool,

    /// Log filter, e.g. `debug` or `apprentice_client=trace,info`.
    #[arg(long, global = true, value_name = "LEVEL")]
    log_level: Option<String>,

    /// Fail (exit 3) instead of starting a daemon when none is running.
    #[arg(long, global = true)]
    no_spawn: bool,

    /// Seconds to wait for each daemon reply (0 = forever).
    #[arg(long, global = true, value_name = "S", default_value_t = 60)]
    timeout: u64,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start, stop or inspect the background daemon.
    #[command(subcommand)]
    Daemon(daemon::DaemonCommand),
    /// Read and write configuration.
    #[command(subcommand)]
    Config(config::ConfigCommand),
    /// Provider API keys.
    #[command(subcommand)]
    Auth(auth::AuthCommand),
    /// Sessions group agent runs on one workspace.
    #[command(subcommand, visible_alias = "s")]
    Session(session::SessionCommand),
    /// Run one prompt through the mentor and stream the answer.
    Run(run::RunArgs),
    /// Inspect the trace store.
    #[command(subcommand)]
    Trace(trace::TraceCommand),
    /// Token usage and cost.
    #[command(subcommand)]
    Stats(stats::StatsCommand),
    /// The tools the mentor can call.
    #[command(subcommand)]
    Tools(tools::ToolsCommand),
    /// The system prompt the mentor gets.
    #[command(subcommand)]
    Prompt(prompt::PromptCommand),
    /// Registered workspace roots.
    #[command(subcommand, visible_alias = "ws")]
    Workspace(workspace::WorkspaceCommand),
    /// Print environment, paths, versions, daemon and hardware facts for bug reports.
    Doctor,
    /// Print a shell completion script
    ///
    /// bash:        harness completions bash > ~/.local/share/bash-completion/completions/harness
    /// zsh:         harness completions zsh > "${fpath[1]}/_harness"
    /// fish:        harness completions fish > ~/.config/fish/completions/harness.fish
    /// powershell:  harness completions powershell >> $PROFILE
    #[command(verbatim_doc_comment)]
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

/// What every command needs: paths, output conventions, and how to reach
/// the daemon.
#[derive(Debug)]
pub struct Ctx {
    pub paths: Paths,
    /// `--home` as given by the user (not the platform default), passed on
    /// to a spawned daemon.
    pub home: Option<PathBuf>,
    pub out: Out,
    pub no_spawn: bool,
    pub log_level: Option<String>,
    /// Per-request timeout; `None` = forever.
    pub timeout: Option<Duration>,
}

impl Ctx {
    pub fn client_options(&self) -> ClientOptions {
        ClientOptions {
            timeout: self.timeout,
            ..ClientOptions::default()
        }
    }
}

fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // `--help` / `--version` go to stdout and are not errors.
            let code = if e.use_stderr() {
                exit::USAGE
            } else {
                exit::OK
            };
            let _ = e.print();
            std::process::exit(code);
        }
    };
    let out = Out::new(cli.json, cli.quiet);
    let code = match run(cli, out) {
        Ok(()) => exit::OK,
        Err(e) => {
            out.report_error(&e);
            code_for(&e)
        }
    };
    out::flush();
    std::process::exit(code);
}

fn run(cli: Cli, out: Out) -> anyhow::Result<()> {
    if let Command::Completions { shell } = cli.command {
        clap_complete::generate(shell, &mut Cli::command(), NAME, &mut std::io::stdout());
        return Ok(());
    }

    // `cli.home` is set by `--home` or `HARNESS_HOME`, never by the
    // platform default, so it is safe to hand to a spawned daemon.
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
    // stop `doctor` from reporting exactly that. Log lines reach stderr
    // only when a level was asked for; the file gets them regardless.
    let explicit_level = cli.log_level.is_some() || telemetry::level_from_env().is_some();
    let log = telemetry::init(
        &telemetry::Options::new("cli", level, paths.data_dir.join("logs"))
            .stderr(explicit_level && !cli.quiet),
    );
    if let Err(e) = &log {
        out.info(format!("warning: logging disabled: {e}"));
    }

    let ctx = Ctx {
        paths,
        home: cli.home.clone(),
        out,
        no_spawn: cli.no_spawn,
        log_level: cli.log_level.clone(),
        timeout: (cli.timeout > 0).then(|| Duration::from_secs(cli.timeout)),
    };
    match cli.command {
        Command::Daemon(cmd) => daemon::run(&ctx, &cmd),
        Command::Config(cmd) => config::run(&ctx, &cmd),
        Command::Auth(cmd) => auth::run(&ctx, &cmd),
        Command::Session(cmd) => session::run(&ctx, &cmd),
        Command::Run(args) => run::run(&ctx, &args),
        Command::Trace(cmd) => trace::run(&ctx, &cmd),
        Command::Stats(cmd) => stats::run(&ctx, &cmd),
        Command::Tools(cmd) => tools::run(&ctx, &cmd),
        Command::Prompt(cmd) => prompt::run(&ctx, &cmd),
        Command::Workspace(cmd) => workspace::run(&ctx, &cmd),
        Command::Doctor => doctor::run(&ctx.paths, log.ok().as_ref(), ctx.out.json),
        Command::Completions { .. } => unreachable!("handled above"),
    }
}

/// Maps an error to the exit-code contract.
fn code_for(e: &anyhow::Error) -> i32 {
    for cause in e.chain() {
        if let Some(x) = cause.downcast_ref::<Exit>() {
            return match x {
                Exit::Interrupted => exit::INTERRUPTED,
                Exit::Usage(_) => exit::USAGE,
            };
        }
        if let Some(c) = cause.downcast_ref::<ConnectError>() {
            return match c {
                ConnectError::Incompatible { .. } => exit::UNAUTHORIZED,
                ConnectError::Client(inner) => code_for_client(inner),
                ConnectError::Discovery(_)
                | ConnectError::NotRunning { .. }
                | ConnectError::DaemonNotFound { .. }
                | ConnectError::Spawn { .. }
                | ConnectError::DaemonExited { .. }
                | ConnectError::SpawnTimeout { .. } => exit::DAEMON_UNAVAILABLE,
            };
        }
        if let Some(c) = cause.downcast_ref::<ClientError>() {
            return code_for_client(c);
        }
    }
    exit::COMMAND_FAILED
}

fn code_for_client(e: &ClientError) -> i32 {
    match e {
        ClientError::Rpc(r) if matches!(r.kind(), Some("unauthorized" | "incompatible_api")) => {
            exit::UNAUTHORIZED
        }
        ClientError::Closed | ClientError::Io(_) => exit::DAEMON_UNAVAILABLE,
        ClientError::Rpc(_) | ClientError::Protocol(_) | ClientError::Timeout(_) => {
            exit::COMMAND_FAILED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apprentice_api::jsonrpc::RpcError;
    use clap::Parser;

    #[test]
    fn name_is_stable() {
        assert_eq!(NAME, "harness");
        Cli::command().debug_assert();
    }

    #[test]
    fn global_flags_parse_before_or_after_the_subcommand() {
        let c =
            Cli::try_parse_from(["harness", "doctor", "--json", "--log-level", "debug"]).unwrap();
        assert!(c.json);
        assert_eq!(c.log_level.as_deref(), Some("debug"));
        assert!(matches!(c.command, Command::Doctor));
        let c = Cli::try_parse_from(["harness", "--no-spawn", "--timeout", "5", "session", "list"])
            .unwrap();
        assert!(c.no_spawn);
        assert_eq!(c.timeout, 5);
        assert!(matches!(c.command, Command::Session(_)));
    }

    #[test]
    fn stats_tokens_flags_parse() {
        let c = Cli::try_parse_from([
            "harness",
            "stats",
            "tokens",
            "--since",
            "7d",
            "--by",
            "day",
            "--by",
            "session",
            "--session",
            "abc",
        ])
        .unwrap();
        assert!(matches!(
            c.command,
            Command::Stats(stats::StatsCommand::Tokens(_))
        ));
        let c = Cli::try_parse_from(["harness", "stats", "reprice", "--model", "m"]).unwrap();
        assert!(matches!(
            c.command,
            Command::Stats(stats::StatsCommand::Reprice(_))
        ));
    }

    #[test]
    fn tools_list_parses() {
        let c = Cli::try_parse_from(["harness", "tools", "list", "--workspace", ".", "--describe"])
            .unwrap();
        assert!(matches!(
            c.command,
            Command::Tools(tools::ToolsCommand::List(_))
        ));
    }

    #[test]
    fn tools_rule_commands_parse() {
        let c = Cli::try_parse_from([
            "harness",
            "tools",
            "allow",
            "shell",
            "--command-prefix",
            "cargo test",
            "--user",
        ])
        .unwrap();
        assert!(matches!(
            c.command,
            Command::Tools(tools::ToolsCommand::Allow(_))
        ));
        let c =
            Cli::try_parse_from(["harness", "tools", "deny", "read_file", "--outside"]).unwrap();
        assert!(matches!(
            c.command,
            Command::Tools(tools::ToolsCommand::Deny(_))
        ));
        assert!(
            Cli::try_parse_from(["harness", "tools", "deny", "x", "--outside", "--inside"])
                .is_err()
        );
        let c = Cli::try_parse_from(["harness", "tools", "rules", "--user"]).unwrap();
        assert!(matches!(
            c.command,
            Command::Tools(tools::ToolsCommand::Rules(_))
        ));
        let c = Cli::try_parse_from(["harness", "run", "--permission-mode", "plan", "x"]).unwrap();
        assert!(matches!(c.command, Command::Run(_)));
        assert!(Cli::try_parse_from(["harness", "run", "--permission-mode", "yolo", "x"]).is_err());
    }

    #[test]
    fn prompt_commands_parse() {
        let c = Cli::try_parse_from(["harness", "prompt", "show", "--session", "s1", "--count"])
            .unwrap();
        assert!(matches!(
            c.command,
            Command::Prompt(prompt::PromptCommand::Show(_))
        ));
        assert!(Cli::try_parse_from(["harness", "prompt", "show", "--workspace", "."]).is_ok());
        assert!(
            Cli::try_parse_from([
                "harness",
                "prompt",
                "show",
                "--session",
                "s1",
                "--workspace",
                "."
            ])
            .is_err()
        );
    }

    #[test]
    fn workspace_commands_parse() {
        let c = Cli::try_parse_from(["harness", "workspace", "add", ".", "--name", "x"]).unwrap();
        assert!(matches!(
            c.command,
            Command::Workspace(workspace::WorkspaceCommand::Add(_))
        ));
        let c = Cli::try_parse_from(["harness", "ws", "info"]).unwrap();
        assert!(matches!(
            c.command,
            Command::Workspace(workspace::WorkspaceCommand::Info(_))
        ));
        let c = Cli::try_parse_from(["harness", "ws", "remove", "id1"]).unwrap();
        assert!(matches!(
            c.command,
            Command::Workspace(workspace::WorkspaceCommand::Remove(_))
        ));
        assert!(Cli::try_parse_from(["harness", "ws", "remove"]).is_err());
    }

    #[test]
    fn run_session_and_workspace_conflict() {
        let e = Cli::try_parse_from(["harness", "run", "hi", "--session", "s", "--workspace", "."])
            .unwrap_err();
        assert_eq!(e.kind(), clap::error::ErrorKind::ArgumentConflict);
        let e =
            Cli::try_parse_from(["harness", "run", "hi", "--session", "s", "--last"]).unwrap_err();
        assert_eq!(e.kind(), clap::error::ErrorKind::ArgumentConflict);
        let c =
            Cli::try_parse_from(["harness", "run", "--last", "--workspace", ".", "hi"]).unwrap();
        assert!(matches!(c.command, Command::Run(_)));
    }

    #[test]
    fn session_commands_parse() {
        for args in [
            vec![
                "session",
                "list",
                "--query",
                "hello",
                "--all",
                "--workspace",
                ".",
            ],
            vec![
                "session", "show", "s1", "--after", "3", "--limit", "10", "--full",
            ],
            vec!["session", "show", "s1", "--tail", "--limit", "10"],
            vec!["session", "search", "hello world", "--limit", "5"],
            vec!["session", "rename", "s1", "A title"],
            vec!["session", "archive", "s1", "--undo"],
            vec!["session", "delete", "s1", "--purge-traces"],
            vec!["session", "export", "s1", "-o", "out.json"],
        ] {
            let mut full = vec!["harness"];
            full.extend(args.iter());
            let c = Cli::try_parse_from(&full).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert!(matches!(c.command, Command::Session(_)));
        }
        assert!(Cli::try_parse_from(["harness", "session", "rename", "s1"]).is_err());
        assert!(
            Cli::try_parse_from(["harness", "session", "show", "s1", "--tail", "--after", "2"])
                .is_err()
        );
    }

    #[test]
    fn exit_codes_follow_the_contract() {
        let not_running: anyhow::Error = ConnectError::NotRunning {
            info_file: PathBuf::from("x"),
            state: "not found",
        }
        .into();
        assert_eq!(code_for(&not_running), exit::DAEMON_UNAVAILABLE);
        assert_eq!(
            code_for(&not_running.context("listing sessions")),
            exit::DAEMON_UNAVAILABLE
        );
        let incompatible: anyhow::Error = ConnectError::Incompatible {
            client_api: 2,
            daemon_api: 3,
            hint: "update",
        }
        .into();
        assert_eq!(code_for(&incompatible), exit::UNAUTHORIZED);
        let unauthorized: anyhow::Error =
            ConnectError::Client(ClientError::Rpc(RpcError::unauthorized("bad token"))).into();
        assert_eq!(code_for(&unauthorized), exit::UNAUTHORIZED);
        let not_found: anyhow::Error = ClientError::Rpc(RpcError::not_found("nope")).into();
        assert_eq!(code_for(&not_found), exit::COMMAND_FAILED);
        let closed: anyhow::Error = ClientError::Closed.into();
        assert_eq!(code_for(&closed), exit::DAEMON_UNAVAILABLE);
        assert_eq!(code_for(&Exit::Interrupted.into()), exit::INTERRUPTED);
        assert_eq!(code_for(&anyhow::anyhow!("other")), exit::COMMAND_FAILED);
    }
}
