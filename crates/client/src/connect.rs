//! Connecting by discovery: read `daemon.json`, connect and shake hands;
//! when nothing answers, spawn `harnessd` detached and wait for it (task
//! M00-08). An older daemon (lower `api_version`) is asked to stop and
//! replaced.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use apprentice_api::API_VERSION;
use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::{DaemonShutdown, HelloResult, ShutdownParams};
use tracing::{debug, info, warn};

use crate::discovery::{DaemonInfo, DiscoveryError};
use crate::{ClientError, ClientOptions, DaemonClient};

/// Environment variable naming the daemon binary explicitly.
pub const DAEMON_PATH_ENV: &str = "HARNESS_DAEMON_PATH";
/// Binary name (`harnessd` / `harnessd.exe`).
pub const DAEMON_BIN: &str = "harnessd";
/// Exit code of a daemon that found another instance holding the lock.
pub const EXIT_ALREADY_RUNNING: i32 = 3;

/// Time allowed for one connect + handshake attempt.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);
/// Poll interval while waiting for a spawned daemon.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How to find or start the daemon.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// Directory holding `daemon.json`.
    pub data_dir: PathBuf,
    /// Explicit `--home` for a spawned daemon. `None` lets the daemon
    /// discover its paths the same way this process did (`HARNESS_HOME` is
    /// inherited, platform directories otherwise).
    pub home: Option<PathBuf>,
    /// Start a daemon when none is running (or the running one is older).
    pub spawn_if_missing: bool,
    /// How long to wait for a spawned daemon to answer.
    pub spawn_timeout: Duration,
    /// Daemon binary; otherwise located next to this executable, then via
    /// [`DAEMON_PATH_ENV`], then on `PATH`.
    pub daemon_path: Option<PathBuf>,
    /// Client name for the handshake.
    pub client_name: String,
    /// Client version for the handshake.
    pub client_version: String,
    pub client: ClientOptions,
}

impl ConnectOptions {
    /// Defaults: spawn when missing, 10 s spawn timeout, binary located
    /// automatically, client `apprentice-client`.
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            home: None,
            spawn_if_missing: true,
            spawn_timeout: Duration::from_secs(10),
            daemon_path: None,
            client_name: "apprentice-client".into(),
            client_version: crate::VERSION.into(),
            client: ClientOptions::default(),
        }
    }

    #[must_use]
    pub fn home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }

    #[must_use]
    pub fn spawn_if_missing(mut self, on: bool) -> Self {
        self.spawn_if_missing = on;
        self
    }

    #[must_use]
    pub fn spawn_timeout(mut self, timeout: Duration) -> Self {
        self.spawn_timeout = timeout;
        self
    }

    #[must_use]
    pub fn daemon_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.daemon_path = Some(path.into());
        self
    }

    #[must_use]
    pub fn client_name(mut self, name: &str, version: &str) -> Self {
        name.clone_into(&mut self.client_name);
        version.clone_into(&mut self.client_version);
        self
    }

    #[must_use]
    pub fn client_options(mut self, options: ClientOptions) -> Self {
        self.client = options;
        self
    }
}

/// A connected, authenticated client plus what is known about the daemon.
#[derive(Debug)]
pub struct Connected {
    pub client: DaemonClient,
    pub hello: HelloResult,
    pub info: DaemonInfo,
    /// This call started the daemon.
    pub spawned: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    /// No daemon answers and spawning is disabled.
    #[error("no daemon is running ({info_file} {state}); start one with `harness daemon start`")]
    NotRunning {
        info_file: PathBuf,
        /// `not found` or `stale`.
        state: &'static str,
    },
    #[error("cannot find the `{DAEMON_BIN}` binary (looked at {})", searched.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))]
    DaemonNotFound { searched: Vec<PathBuf> },
    #[error("cannot start {}: {source}", path.display())]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The spawned daemon exited before answering.
    #[error("daemon {} exited with {code} before answering", path.display())]
    DaemonExited { path: PathBuf, code: String },
    #[error("daemon did not answer within {timeout:?}{}", last_error.as_ref().map(|e| format!(" (last error: {e})")).unwrap_or_default())]
    SpawnTimeout {
        timeout: Duration,
        last_error: Option<String>,
    },
    /// The daemon speaks a different API and cannot be replaced (newer than
    /// this client, or spawning disabled).
    #[error("daemon speaks API v{daemon_api}, this client v{client_api}; {hint}")]
    Incompatible {
        client_api: u32,
        daemon_api: u32,
        hint: &'static str,
    },
    /// The handshake failed for another reason (wrong token, ...).
    #[error(transparent)]
    Client(#[from] ClientError),
}

/// Outcome of one connect + handshake attempt against a `daemon.json`.
enum Attempt {
    /// Nothing listening, or the transport broke: treat the file as stale.
    Unreachable(String),
    /// The daemon answered the handshake with an error.
    Rejected(RpcError),
}

impl DaemonClient {
    /// Finds the daemon through `daemon.json`, spawning one when needed,
    /// and performs the handshake.
    ///
    /// # Errors
    /// See [`ConnectError`].
    pub async fn connect(options: &ConnectOptions) -> Result<Connected, ConnectError> {
        let info_file = DaemonInfo::path(&options.data_dir);
        let mut state = "not found";
        let mut stale_pid = None;
        if let Some(info) = DaemonInfo::read(&options.data_dir)? {
            stale_pid = Some(info.pid);
            match attempt(&info, options).await {
                Ok((client, hello)) => {
                    return Ok(Connected {
                        client,
                        hello,
                        info,
                        spawned: false,
                    });
                }
                Err(Attempt::Unreachable(e)) => {
                    debug!(pid = info.pid, endpoint = %info.endpoint, error = e, "daemon.json is stale");
                    state = "stale";
                }
                Err(Attempt::Rejected(e)) if e.kind() == Some("incompatible_api") => {
                    let daemon_api = daemon_api_from(&e).unwrap_or(info.api_version);
                    if daemon_api >= API_VERSION {
                        return Err(ConnectError::Incompatible {
                            client_api: API_VERSION,
                            daemon_api,
                            hint: "update this client",
                        });
                    }
                    if !options.spawn_if_missing {
                        return Err(ConnectError::Incompatible {
                            client_api: API_VERSION,
                            daemon_api,
                            hint: "restart the daemon (`harness daemon stop`, then start)",
                        });
                    }
                    info!(pid = info.pid, daemon_api, "replacing an older daemon");
                    replace_older(&info, options).await;
                    state = "replaced";
                }
                Err(Attempt::Rejected(e)) => return Err(ClientError::Rpc(e).into()),
            }
        }
        if !options.spawn_if_missing {
            return Err(ConnectError::NotRunning { info_file, state });
        }
        let path = locate_daemon(options.daemon_path.as_deref())?;
        let mut child = spawn_detached(&path, options)?;
        info!(pid = child.id(), path = %path.display(), "daemon spawned");

        let deadline = Instant::now() + options.spawn_timeout;
        let mut last_error = None;
        loop {
            if let Some(status) = child.try_wait().ok().flatten() {
                // Exit 3 means another daemon won the lock; keep polling for
                // that one. Anything else is a startup failure.
                if status.code() != Some(EXIT_ALREADY_RUNNING) {
                    return Err(ConnectError::DaemonExited {
                        path,
                        code: status.to_string(),
                    });
                }
                // The daemon named in the file is alive after all.
                stale_pid = None;
            }
            if let Some(info) = DaemonInfo::read(&options.data_dir)? {
                // The endpoint name is derived from the data dir, so the
                // new daemon listens on it before rewriting `daemon.json`:
                // an attempt with the old file's token would be rejected
                // as `unauthorized`. Wait for the file to change.
                if Some(info.pid) == stale_pid {
                    last_error = Some("daemon.json still names the old daemon".into());
                } else {
                    match attempt(&info, options).await {
                        Ok((client, hello)) => {
                            return Ok(Connected {
                                client,
                                hello,
                                info,
                                spawned: true,
                            });
                        }
                        Err(Attempt::Unreachable(e)) => last_error = Some(e),
                        Err(Attempt::Rejected(e)) => return Err(ClientError::Rpc(e).into()),
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err(ConnectError::SpawnTimeout {
                    timeout: options.spawn_timeout,
                    last_error,
                });
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

async fn attempt(
    info: &DaemonInfo,
    options: &ConnectOptions,
) -> Result<(DaemonClient, HelloResult), Attempt> {
    let connect = DaemonClient::connect_endpoint(&info.endpoint, options.client.clone());
    let client = match tokio::time::timeout(ATTEMPT_TIMEOUT, connect).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return Err(Attempt::Unreachable(e.to_string())),
        Err(_) => return Err(Attempt::Unreachable("connect timed out".into())),
    };
    let hello = client.hello(
        &options.client_name,
        &options.client_version,
        Some(info.token.clone()),
    );
    match tokio::time::timeout(ATTEMPT_TIMEOUT, hello).await {
        Ok(Ok(h)) => Ok((client, h)),
        Ok(Err(ClientError::Rpc(e))) => Err(Attempt::Rejected(e)),
        Ok(Err(e)) => Err(Attempt::Unreachable(e.to_string())),
        Err(_) => Err(Attempt::Unreachable("handshake timed out".into())),
    }
}

fn daemon_api_from(e: &RpcError) -> Option<u32> {
    e.data
        .as_ref()?
        .details
        .as_ref()?
        .get("daemon_api")?
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
}

/// Asks the daemon described by `old` to stop (token-authorised, no
/// handshake needed) and waits until it no longer answers. Best effort: on
/// timeout the caller spawns anyway and the lock decides.
async fn replace_older(old: &DaemonInfo, options: &ConnectOptions) {
    if let Ok(Ok(client)) = tokio::time::timeout(
        ATTEMPT_TIMEOUT,
        DaemonClient::connect_endpoint(&old.endpoint, options.client.clone()),
    )
    .await
    {
        let shutdown = client.call::<DaemonShutdown>(ShutdownParams {
            graceful: true,
            token: Some(old.token.clone()),
        });
        match tokio::time::timeout(ATTEMPT_TIMEOUT, shutdown).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => warn!(error = %e, "older daemon refused to stop"),
            Err(_) => warn!("older daemon did not answer daemon.shutdown"),
        }
    }
    let deadline = Instant::now() + options.spawn_timeout;
    while Instant::now() < deadline {
        let gone = match DaemonInfo::read(&options.data_dir) {
            Ok(Some(info)) if info.pid == old.pid => old.endpoint.connect().await.is_err(),
            _ => true,
        };
        if gone {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    warn!(
        pid = old.pid,
        "older daemon still running; starting a new one anyway"
    );
}

/// Finds `harnessd`: `explicit`, next to the current executable,
/// [`DAEMON_PATH_ENV`], then `PATH`.
///
/// # Errors
/// [`ConnectError::DaemonNotFound`] listing every location tried.
pub fn locate_daemon(explicit: Option<&Path>) -> Result<PathBuf, ConnectError> {
    let bin = format!("{DAEMON_BIN}{}", std::env::consts::EXE_SUFFIX);
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = explicit {
        candidates.push(p.to_path_buf());
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        candidates.push(dir.join(&bin));
    }
    if let Some(p) = std::env::var_os(DAEMON_PATH_ENV).filter(|p| !p.is_empty()) {
        candidates.push(PathBuf::from(p));
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join(&bin)));
    }
    candidates
        .iter()
        .find(|p| p.is_file())
        .cloned()
        .ok_or(ConnectError::DaemonNotFound {
            searched: candidates,
        })
}

/// Starts the daemon detached from this process: no inherited console or
/// stdio, its own process group, stderr appended to
/// `<data_dir>/logs/daemon.stderr.log` for anything printed before logging
/// is up (panics included).
fn spawn_detached(
    path: &Path,
    options: &ConnectOptions,
) -> Result<std::process::Child, ConnectError> {
    let mut cmd = std::process::Command::new(path);
    if let Some(home) = &options.home {
        cmd.arg("--home").arg(home);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null());
    let logs = options.data_dir.join("logs");
    let stderr = std::fs::create_dir_all(&logs).and_then(|()| {
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(logs.join("daemon.stderr.log"))
    });
    match stderr {
        Ok(f) => cmd.stderr(Stdio::from(f)),
        Err(_) => cmd.stderr(Stdio::null()),
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // DETACHED_PROCESS: no console at all (so closing the CLI's console
        // cannot reach it); CREATE_NEW_PROCESS_GROUP: not in the CLI's
        // CTRL-C group; CREATE_NO_WINDOW is implied by DETACHED_PROCESS.
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        stop_inheriting_std_handles();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Own process group: the terminal's SIGINT/SIGHUP stay with the CLI.
        cmd.process_group(0);
    }
    cmd.spawn().map_err(|source| ConnectError::Spawn {
        path: path.to_path_buf(),
        source,
    })
}

/// Marks this process's stdio handles non-inheritable. A child gets every
/// inheritable handle of its parent (`CreateProcess` is called with
/// `bInheritHandles = TRUE` for the stdio it is given), so without this
/// the daemon would hold the pipes a shell or a test gave *us* and
/// `$(harness session new)` would wait for EOF until the daemon exits.
/// Later children that inherit our stdio still work: the standard library
/// duplicates the handle as inheritable for them.
#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "SetHandleInformation has no safe wrapper; called on this process's own std handles"
)]
fn stop_inheriting_std_handles() {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    for handle in [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ] {
        if handle.is_null() {
            continue;
        }
        // SAFETY: `handle` is a valid handle owned by this process for as
        // long as the process lives; the call only changes its flags.
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_prefers_explicit_then_reports_everything_tried() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("harnessd-fake");
        std::fs::write(&fake, b"").unwrap();
        assert_eq!(locate_daemon(Some(&fake)).unwrap(), fake);

        let missing = dir.path().join("nope");
        // `explicit` is only a candidate; the search continues elsewhere.
        match locate_daemon(Some(&missing)) {
            Err(ConnectError::DaemonNotFound { searched }) => {
                assert_eq!(searched[0], missing);
                assert!(searched.len() > 1, "{searched:?}");
            }
            // A real harnessd next to the test binary or on PATH: fine too.
            Ok(p) => assert!(p.is_file()),
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn daemon_api_is_read_from_error_details() {
        let e = RpcError::incompatible_api(1, 0);
        assert_eq!(daemon_api_from(&e), Some(0));
        assert_eq!(daemon_api_from(&RpcError::internal("x")), None);
    }
}
