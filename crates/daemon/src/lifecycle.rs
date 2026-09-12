//! The daemon's life: serve the local socket (or one stdio connection),
//! answer `daemon.status` / `daemon.shutdown`, exit on signals or idleness,
//! and shut down in order — stop accepting, cancel agents, let in-flight
//! requests answer, flush the trace writer, remove `daemon.json`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::{
    DaemonShutdown, DaemonStatus, DaemonStatusResult, Empty, ShutdownParams,
};
use apprentice_api::server::{Connection, Router, RouterConfig};
use apprentice_api::transport::{Endpoint, LocalListener};
use apprentice_client::DaemonInfo;
use apprentice_core::app::AppState;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{debug, error, info, warn};

/// Everything must be over this long after shutdown starts; then the
/// process exits regardless.
pub const HARD_DEADLINE: Duration = Duration::from_secs(10);
/// Time given to open connections to finish their in-flight requests.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// What `main` decided from flags and config.
#[derive(Debug, Clone)]
pub struct Options {
    /// Exit after this long without clients; `None` = never.
    pub idle: Option<Duration>,
    /// Reported by `daemon.status`.
    pub log_file: Option<PathBuf>,
}

/// Per-process serving state shared by the accept loop and the handlers.
struct Server {
    state: Arc<AppState>,
    started: Instant,
    log_file: Option<String>,
    connections: AtomicUsize,
    /// When the connection count last dropped to zero (or the start).
    idle_since: Mutex<Instant>,
    /// First shutdown reason wins; reported in the log.
    reason: Mutex<Option<&'static str>>,
    /// `daemon.shutdown{graceful: false}`: skip draining connections.
    forced: AtomicBool,
}

impl Server {
    fn shutdown(&self) -> &CancellationToken {
        self.state.shutdown()
    }

    fn request_shutdown(&self, reason: &'static str) {
        let mut slot = self
            .reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(reason);
            info!(reason, "shutdown requested");
        }
        drop(slot);
        self.shutdown().cancel();
    }

    fn reason(&self) -> &'static str {
        self.reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .unwrap_or("unknown")
    }

    fn connected(&self) {
        let n = self.connections.fetch_add(1, Ordering::AcqRel) + 1;
        debug!(connections = n, "client connected");
    }

    fn disconnected(&self) {
        let n = self.connections.fetch_sub(1, Ordering::AcqRel) - 1;
        debug!(connections = n, "client disconnected");
        if n == 0 {
            *self
                .idle_since
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
        }
    }

    /// No clients for at least `idle`. Running agents count as activity
    /// once they exist (M00-11).
    fn idle_for(&self, idle: Duration) -> bool {
        self.connections.load(Ordering::Acquire) == 0
            && self
                .idle_since
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .elapsed()
                >= idle
    }

    fn status(&self) -> Result<DaemonStatusResult, RpcError> {
        Ok(DaemonStatusResult {
            version: apprentice_core::VERSION.into(),
            pid: std::process::id(),
            uptime_s: self.started.elapsed().as_secs(),
            sessions_open: self.state.sessions_open()?,
            data_dir: self.state.paths().data_dir.display().to_string(),
            log_file: self.log_file.clone(),
        })
    }

    /// `daemon.*` on top of the core handlers.
    fn router(self: &Arc<Self>, token: Option<String>) -> Arc<Router> {
        let mut router = Router::new(RouterConfig {
            daemon_version: apprentice_core::VERSION.into(),
            pid: std::process::id(),
            token,
        });
        self.state.register(&mut router);
        let server = Arc::clone(self);
        router.add::<DaemonStatus, _, _>(move |_c: Arc<Connection>, Empty {}| {
            let server = Arc::clone(&server);
            async move {
                tokio::task::spawn_blocking(move || server.status())
                    .await
                    .map_err(|e| RpcError::internal(format!("status task failed: {e}")))?
            }
        });
        let server = Arc::clone(self);
        router.add::<DaemonShutdown, _, _>(move |c: Arc<Connection>, p: ShutdownParams| {
            let server = Arc::clone(&server);
            async move {
                if !p.graceful {
                    server.forced.store(true, Ordering::Release);
                }
                info!(conn = c.id, graceful = p.graceful, "daemon.shutdown");
                // The reply is still written: the connection drains its
                // in-flight responses before closing.
                server.request_shutdown("rpc");
                Ok(Empty {})
            }
        });
        Arc::new(router)
    }
}

/// Serves the local socket until shutdown. `daemon.json` is written once
/// the listener is bound and removed on the way out.
///
/// # Errors
/// Binding the socket or writing `daemon.json` failed.
pub async fn serve_socket(state: Arc<AppState>, options: Options) -> anyhow::Result<()> {
    let data_dir = state.paths().data_dir.clone();
    let endpoint = Endpoint::default_for(&data_dir, &instance_key(&data_dir));
    let listener = endpoint
        .listen()
        .with_context(|| format!("cannot listen on {endpoint}"))?;
    let token = new_token();
    let info = DaemonInfo {
        pid: std::process::id(),
        endpoint: endpoint.clone(),
        token: token.clone(),
        api_version: apprentice_api::API_VERSION,
        version: apprentice_core::VERSION.into(),
        started_at: apprentice_core::trace::now_ts(),
    };
    info.write(&data_dir)
        .with_context(|| format!("cannot write {}", DaemonInfo::path(&data_dir).display()))?;
    info!(endpoint = %endpoint, info = %DaemonInfo::path(&data_dir).display(), "listening");

    let server = new_server(state, &options);
    let router = server.router(Some(token));
    spawn_watchers(&server, options.idle);

    let tracker = TaskTracker::new();
    accept_loop(&server, &router, &listener, &tracker).await;
    // Stop accepting before anything else; on Unix this also removes the
    // socket file.
    drop(listener);

    let result = finish(&server, &tracker).await;
    if let Err(e) = DaemonInfo::remove(&data_dir) {
        warn!(error = %e, "cannot remove daemon.json");
    }
    result
}

/// Serves exactly one connection over stdin/stdout, then exits. No lock,
/// no `daemon.json`, no token: the peer that started us is the only
/// client.
///
/// # Errors
/// Never at present; kept for parity with [`serve_socket`].
pub async fn serve_stdio(state: Arc<AppState>, options: Options) -> anyhow::Result<()> {
    let server = new_server(state, &options);
    let router = server.router(None);
    spawn_watchers(&server, options.idle);
    server.connected();
    let connection = Arc::clone(&router).serve_with_shutdown(
        tokio::io::stdin(),
        tokio::io::stdout(),
        server.shutdown().clone().cancelled_owned(),
    );
    if let Err(e) = connection.await {
        warn!(error = %e, "stdio connection failed");
    }
    server.disconnected();
    server.request_shutdown("stdio closed");
    finish(&server, &TaskTracker::new()).await
}

fn new_server(state: Arc<AppState>, options: &Options) -> Arc<Server> {
    Arc::new(Server {
        state,
        started: Instant::now(),
        log_file: options.log_file.as_ref().map(|p| p.display().to_string()),
        connections: AtomicUsize::new(0),
        idle_since: Mutex::new(Instant::now()),
        reason: Mutex::new(None),
        forced: AtomicBool::new(false),
    })
}

/// Signal handling and the idle timer.
fn spawn_watchers(server: &Arc<Server>, idle: Option<Duration>) {
    let s = Arc::clone(server);
    tokio::spawn(async move {
        tokio::select! {
            reason = wait_for_signal() => s.request_shutdown(reason),
            () = s.shutdown().cancelled() => {}
        }
    });
    if let Some(idle) = idle {
        let s = Arc::clone(server);
        tokio::spawn(async move {
            let tick = (idle / 4).clamp(Duration::from_millis(100), Duration::from_secs(5));
            loop {
                tokio::select! {
                    () = tokio::time::sleep(tick) => {}
                    () = s.shutdown().cancelled() => return,
                }
                if s.idle_for(idle) {
                    s.request_shutdown("idle");
                    return;
                }
            }
        });
    }
}

async fn accept_loop(
    server: &Arc<Server>,
    router: &Arc<Router>,
    listener: &LocalListener,
    tracker: &TaskTracker,
) {
    loop {
        let accepted = tokio::select! {
            () = server.shutdown().cancelled() => break,
            accepted = listener.accept() => accepted,
        };
        match accepted {
            Ok((reader, writer)) => {
                server.connected();
                let server = Arc::clone(server);
                let router = Arc::clone(router);
                tracker.spawn(async move {
                    let stop = server.shutdown().clone().cancelled_owned();
                    if let Err(e) = router.serve_with_shutdown(reader, writer, stop).await {
                        debug!(error = %e, "connection ended with an error");
                    }
                    server.disconnected();
                });
            }
            Err(e) => {
                // Transient (EMFILE, a peer vanishing mid-accept): back off
                // rather than spin.
                warn!(error = %e, "accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// The ordered shutdown, after accepting stopped: cancel agents (they share
/// the token), drain connections, flush traces. A watchdog thread ends the
/// process at [`HARD_DEADLINE`] no matter what.
async fn finish(server: &Arc<Server>, tracker: &TaskTracker) -> anyhow::Result<()> {
    let reason = server.reason();
    info!(
        reason,
        connections = server.connections.load(Ordering::Acquire),
        "shutting down"
    );
    std::thread::Builder::new()
        .name("shutdown-watchdog".into())
        .spawn(|| {
            std::thread::sleep(HARD_DEADLINE);
            error!(deadline = ?HARD_DEADLINE, "shutdown did not finish in time; exiting");
            std::process::exit(1);
        })
        .context("cannot start the shutdown watchdog")?;

    tracker.close();
    if server.forced.load(Ordering::Acquire) {
        info!("forced shutdown: not waiting for connections");
    } else if tokio::time::timeout(DRAIN_TIMEOUT, tracker.wait())
        .await
        .is_err()
    {
        warn!(timeout = ?DRAIN_TIMEOUT, "connections still open; closing anyway");
    }
    server.state.close().await;
    info!(reason, "stopped");
    Ok(())
}

/// The platform's stop signals, resolved to a reason string.
async fn wait_for_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "cannot listen for SIGTERM");
                std::future::pending().await
            }
        };
        let mut hup = match signal(SignalKind::hangup()) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "cannot listen for SIGHUP");
                std::future::pending().await
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => "SIGINT",
            _ = term.recv() => "SIGTERM",
            _ = hup.recv() => "SIGHUP",
        }
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows;
        // Console close / logoff / shutdown give the process a few seconds
        // (tokio keeps the handler thread parked so the OS waits for us).
        let (Ok(mut brk), Ok(mut close), Ok(mut logoff), Ok(mut down)) = (
            windows::ctrl_break(),
            windows::ctrl_close(),
            windows::ctrl_logoff(),
            windows::ctrl_shutdown(),
        ) else {
            warn!("cannot register console control handlers");
            let _ = tokio::signal::ctrl_c().await;
            return "CTRL-C";
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => "CTRL-C",
            _ = brk.recv() => "CTRL-BREAK",
            _ = close.recv() => "console close",
            _ = logoff.recv() => "logoff",
            _ = down.recv() => "system shutdown",
        }
    }
}

/// 256 random bits, hex.
fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// What makes this daemon distinct on the machine: the user and the data
/// directory (two `HARNESS_HOME`s of one user must not share a pipe).
fn instance_key(data_dir: &std::path::Path) -> String {
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "user".into());
    format!("{user}@{}", data_dir.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_long_and_unique() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn instance_key_depends_on_the_data_dir() {
        let a = instance_key(std::path::Path::new("/a"));
        let b = instance_key(std::path::Path::new("/b"));
        assert_ne!(a, b);
        assert!(a.ends_with("@/a"));
    }
}
