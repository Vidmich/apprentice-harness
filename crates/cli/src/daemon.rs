//! Reaching the daemon (`DaemonClient::connect`, spawning one unless
//! `--no-spawn`) and `harness daemon start|stop|status|run`.

use std::time::{Duration, Instant};

use anyhow::Context as _;
use apprentice_api::methods::{DaemonShutdown, DaemonStatus, Empty, ShutdownParams};
use apprentice_client::connect::locate_daemon;
use apprentice_client::{ConnectError, ConnectOptions, Connected, DaemonClient, DaemonInfo};
use clap::{Args, Subcommand};
use serde::Serialize;

use crate::Ctx;

/// How long `daemon stop` waits for the process to go away (the daemon's
/// own hard deadline is 10 s).
const STOP_TIMEOUT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Start the daemon in the background (no-op when one is running).
    Start,
    /// Ask the running daemon to stop and wait until it has.
    Stop(StopArgs),
    /// Show pid, uptime and paths of the running daemon (exit 3 if none).
    Status,
    /// Run the daemon in the foreground, attached to this terminal.
    ///
    /// Useful for development: logs go to stderr and CTRL-C stops it.
    Run,
}

#[derive(Debug, Args)]
pub struct StopArgs {
    /// Do not wait for running agents; cancel them and exit.
    #[arg(long)]
    force: bool,
}

/// `daemon start` / `daemon status` output.
#[derive(Debug, Serialize)]
struct StartReport {
    pid: u32,
    endpoint: String,
    version: String,
    api_version: u32,
    started_at: String,
    /// This command started the daemon.
    spawned: bool,
}

/// Builds the connect options from the global flags. `spawn` overrides
/// `--no-spawn` for commands that must never start a daemon (`stop`,
/// `status`, `doctor`).
pub fn options(ctx: &Ctx, spawn: bool) -> ConnectOptions {
    let mut options = ConnectOptions::new(&ctx.paths.data_dir)
        .spawn_if_missing(spawn && !ctx.no_spawn)
        .client_name("harness-cli", apprentice_client::VERSION)
        .client_options(ctx.client_options());
    if let Some(home) = &ctx.home {
        options = options.home(home);
    }
    options
}

/// Builds the current-thread runtime every command runs on.
pub fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

/// Connects (spawning a daemon unless `--no-spawn`) and runs `f`.
///
/// # Errors
/// [`ConnectError`] (exit 3 / 4), or whatever `f` returns.
pub fn with_client<T, F, Fut>(ctx: &Ctx, f: F) -> anyhow::Result<T>
where
    F: FnOnce(DaemonClient) -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let options = options(ctx, true);
    runtime()?.block_on(async {
        let connected = DaemonClient::connect(&options).await?;
        if connected.spawned {
            ctx.out
                .info(format!("started daemon (pid {})", connected.hello.pid));
        }
        f(connected.client).await
    })
}

pub fn run(ctx: &Ctx, cmd: &DaemonCommand) -> anyhow::Result<()> {
    match cmd {
        DaemonCommand::Start => start(ctx),
        DaemonCommand::Stop(args) => stop(ctx, args),
        DaemonCommand::Status => status(ctx),
        DaemonCommand::Run => run_foreground(ctx),
    }
}

fn report(c: &Connected) -> StartReport {
    StartReport {
        pid: c.hello.pid,
        endpoint: c.info.endpoint.to_string(),
        version: c.hello.daemon_version.clone(),
        api_version: c.hello.api_version,
        started_at: c.info.started_at.clone(),
        spawned: c.spawned,
    }
}

fn start(ctx: &Ctx) -> anyhow::Result<()> {
    // `daemon start` means start: `--no-spawn` does not apply.
    let options = options(ctx, true).spawn_if_missing(true);
    let connected = runtime()?.block_on(DaemonClient::connect(&options))?;
    let r = report(&connected);
    if ctx.out.json {
        ctx.out.emit_json(&r)?;
    } else {
        let verb = if r.spawned {
            "started daemon"
        } else {
            "daemon already running"
        };
        ctx.out.line(format!(
            "{verb} (pid {}, v{}, {})",
            r.pid, r.version, r.endpoint
        ));
    }
    Ok(())
}

fn status(ctx: &Ctx) -> anyhow::Result<()> {
    let options = options(ctx, false);
    let rt = runtime()?;
    let (connected, status) = rt.block_on(async {
        let connected = DaemonClient::connect(&options).await?;
        let status = connected.client.call::<DaemonStatus>(Empty {}).await?;
        anyhow::Ok((connected, status))
    })?;
    if ctx.out.json {
        ctx.out.emit_json(&status)?;
    } else {
        let mut pairs = vec![
            ("pid", status.pid.to_string()),
            ("version", status.version.clone()),
            ("api version", connected.hello.api_version.to_string()),
            ("uptime", human_duration(status.uptime_s)),
            ("sessions", status.sessions_open.to_string()),
            ("endpoint", connected.info.endpoint.to_string()),
            ("data dir", status.data_dir.clone()),
        ];
        if let Some(log) = &status.log_file {
            pairs.push(("log file", log.clone()));
        }
        ctx.out.print_kv(&pairs);
    }
    Ok(())
}

fn stop(ctx: &Ctx, args: &StopArgs) -> anyhow::Result<()> {
    let options = options(ctx, false);
    let rt = runtime()?;
    let outcome = rt.block_on(async {
        let connected = match DaemonClient::connect(&options).await {
            Ok(c) => c,
            Err(ConnectError::NotRunning { .. }) => return anyhow::Ok(None),
            Err(e) => return Err(e.into()),
        };
        let pid = connected.hello.pid;
        connected
            .client
            .call::<DaemonShutdown>(ShutdownParams {
                graceful: !args.force,
                token: None,
            })
            .await
            .context("daemon.shutdown")?;
        drop(connected);
        let deadline = Instant::now() + STOP_TIMEOUT;
        loop {
            let gone = match DaemonInfo::read(&options.data_dir) {
                Ok(Some(info)) if info.pid == pid => info.endpoint.connect().await.is_err(),
                _ => true,
            };
            if gone {
                return Ok(Some((pid, true)));
            }
            if Instant::now() >= deadline {
                return Ok(Some((pid, false)));
            }
            tokio::time::sleep(POLL).await;
        }
    })?;
    match outcome {
        None => {
            ctx.out.info("no daemon is running");
            if ctx.out.json {
                ctx.out
                    .emit_json(&serde_json::json!({ "stopped": false, "running": false }))?;
            }
        }
        Some((pid, true)) => {
            if ctx.out.json {
                ctx.out
                    .emit_json(&serde_json::json!({ "stopped": true, "pid": pid }))?;
            } else {
                ctx.out.line(format!("daemon stopped (pid {pid})"));
            }
        }
        Some((pid, false)) => {
            anyhow::bail!("daemon (pid {pid}) is still running after {STOP_TIMEOUT:?}");
        }
    }
    Ok(())
}

/// Runs `harnessd --foreground` attached to this terminal and exits with
/// its code. CTRL-C reaches the daemon directly (same console / process
/// group); this process just waits for it to finish shutting down.
fn run_foreground(ctx: &Ctx) -> anyhow::Result<()> {
    let path = locate_daemon(None)?;
    let mut cmd = tokio::process::Command::new(&path);
    cmd.arg("--foreground");
    if let Some(home) = &ctx.home {
        cmd.arg("--home").arg(home);
    }
    if let Some(level) = &ctx.log_level {
        cmd.arg("--log-level").arg(level);
    }
    ctx.out.info(format!("running {}", path.display()));
    let status = runtime()?.block_on(async {
        let mut child = cmd
            .spawn()
            .with_context(|| format!("cannot start {}", path.display()))?;
        tokio::select! {
            status = child.wait() => anyhow::Ok(status?),
            _ = tokio::signal::ctrl_c() => {
                ctx.out.info("waiting for the daemon to stop (CTRL-C again to abandon it)");
                tokio::select! {
                    status = child.wait() => Ok(status?),
                    _ = tokio::signal::ctrl_c() => Err(crate::Exit::Interrupted.into()),
                }
            }
        }
    })?;
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => anyhow::bail!("daemon exited with code {code}"),
        None => anyhow::bail!("daemon was killed by a signal"),
    }
}

/// `3h 4m 5s`-style uptime.
pub fn human_duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn durations_are_short() {
        assert_eq!(super::human_duration(5), "5s");
        assert_eq!(super::human_duration(65), "1m 5s");
        assert_eq!(super::human_duration(3600 * 3 + 245), "3h 4m 5s");
    }
}
