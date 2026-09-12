//! Daemon lifecycle against the real `harnessd` binary (task M00-08):
//! single instance, `daemon.json`, client discovery and spawning, older
//! daemon replacement, idle exit, stdio mode, graceful shutdown.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use apprentice_api::API_VERSION;
use apprentice_api::methods::{
    DaemonShutdown, DaemonStatus, Empty, SessionCreate, SessionCreateParams, SessionList,
    SessionListParams, ShutdownParams, TraceList, TraceListParams,
};
use apprentice_api::server::{Router, RouterConfig};
use apprentice_api::transport::Endpoint;
use apprentice_client::{
    ClientError, ClientOptions, ConnectError, ConnectOptions, DaemonClient, DaemonInfo,
};

const BIN: &str = env!("CARGO_BIN_EXE_harnessd");
const WAIT: Duration = Duration::from_secs(15);

fn data_dir(home: &Path) -> PathBuf {
    home.join("data")
}

/// Starts the binary directly (as `harness daemon run` would), detached
/// from our stdio.
fn spawn_daemon(home: &Path, extra: &[&str]) -> Child {
    Command::new(BIN)
        .arg("--home")
        .arg(home)
        .args(extra)
        .env_remove("HARNESS_LOG_LEVEL")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn harnessd")
}

fn wait_exit(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "daemon did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for<F: FnMut() -> bool>(what: &str, mut f: F) {
    let deadline = Instant::now() + WAIT;
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn options(home: &Path) -> ConnectOptions {
    ConnectOptions::new(data_dir(home))
        .home(home)
        .daemon_path(BIN)
        .client_name("lifecycle-test", "0")
}

async fn connect_existing(home: &Path) -> DaemonClient {
    let c = DaemonClient::connect(&options(home).spawn_if_missing(false))
        .await
        .expect("connect");
    assert!(!c.spawned);
    c.client
}

async fn shutdown_and_wait(client: &DaemonClient, home: &Path) {
    client
        .call::<DaemonShutdown>(ShutdownParams::default())
        .await
        .expect("daemon.shutdown answers before the daemon goes");
    let data = data_dir(home);
    wait_for("daemon.json to be removed", || {
        DaemonInfo::read(&data).unwrap().is_none()
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn second_instance_exits_with_code_3_naming_the_pid() {
    let home = tempfile::tempdir().unwrap();
    let mut first = spawn_daemon(home.path(), &[]);
    let data = data_dir(home.path());
    wait_for("daemon.json", || DaemonInfo::read(&data).unwrap().is_some());
    let info = DaemonInfo::read(&data).unwrap().unwrap();
    assert_eq!(info.pid, first.id());
    assert_eq!(info.api_version, API_VERSION);

    let second = Command::new(BIN)
        .arg("--home")
        .arg(home.path())
        .env_remove("HARNESS_LOG_LEVEL")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains(&format!("already running (pid {})", first.id())),
        "{stderr}"
    );
    // The loser did not touch the winner's daemon.json.
    assert_eq!(DaemonInfo::read(&data).unwrap().unwrap(), info);

    let client = connect_existing(home.path()).await;
    shutdown_and_wait(&client, home.path()).await;
    let status = wait_exit(&mut first);
    assert!(status.success(), "{status}");
    assert!(!data.join("daemon.json").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_info_is_ignored_and_the_client_spawns_one_shared_daemon() {
    let home = tempfile::tempdir().unwrap();
    let data = data_dir(home.path());
    std::fs::create_dir_all(&data).unwrap();
    // A dead daemon's leftovers.
    let stale_endpoint = if cfg!(windows) {
        Endpoint::default_for(&data, &format!("stale-{}", std::process::id()))
    } else {
        Endpoint::Path(data.join("stale.sock"))
    };
    DaemonInfo {
        pid: 999_999,
        endpoint: stale_endpoint,
        token: "stale".into(),
        api_version: API_VERSION,
        version: "0.0.0".into(),
        started_at: "2020-01-01T00:00:00.000Z".into(),
    }
    .write(&data)
    .unwrap();

    let started = Instant::now();
    let first = DaemonClient::connect(&options(home.path())).await.unwrap();
    let elapsed = started.elapsed();
    assert!(first.spawned);
    assert_ne!(first.info.pid, 999_999);
    assert_eq!(first.hello.pid, first.info.pid);
    assert_eq!(first.hello.api_version, API_VERSION);
    // Reference: under 2 s; debug builds on a loaded machine get slack.
    assert!(elapsed < Duration::from_secs(8), "spawn took {elapsed:?}");

    let second = DaemonClient::connect(&options(home.path())).await.unwrap();
    assert!(!second.spawned);
    assert_eq!(second.hello.pid, first.hello.pid);

    // The core is wired: sessions, traces, status.
    let created = second
        .client
        .call::<SessionCreate>(SessionCreateParams {
            workspace: None,
            title: Some("lifecycle".into()),
        })
        .await
        .unwrap();
    let sessions = first
        .client
        .call::<SessionList>(SessionListParams::default())
        .await
        .unwrap();
    assert_eq!(sessions.sessions.len(), 1);
    assert_eq!(sessions.sessions[0].id, created.session_id);
    let events = first
        .client
        .call::<TraceList>(TraceListParams {
            session_id: Some(created.session_id.clone()),
            ..TraceListParams::default()
        })
        .await
        .unwrap();
    assert_eq!(events.events.len(), 1);
    assert_eq!(events.events[0].kind, "session.created");
    let status = first.client.call::<DaemonStatus>(Empty {}).await.unwrap();
    assert_eq!(status.pid, first.hello.pid);
    assert_eq!(status.sessions_open, 1);
    assert_eq!(status.data_dir, data.display().to_string());
    assert!(status.log_file.is_some_and(|f| f.contains("daemon.")));
    assert_eq!(status.version, first.hello.daemon_version);

    // Wrong token: unauthorized. No spawn: a clear error once it is gone.
    let raw = DaemonClient::connect_endpoint(&first.info.endpoint, ClientOptions::default())
        .await
        .unwrap();
    let err = raw
        .hello("lifecycle-test", "0", Some("wrong".into()))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.kind() == Some("unauthorized")),
        "{err}"
    );
    drop(raw);

    shutdown_and_wait(&second.client, home.path()).await;
    let deadline = Instant::now() + WAIT;
    while first.info.endpoint.connect().await.is_ok() {
        assert!(Instant::now() < deadline, "endpoint still accepts");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let err = DaemonClient::connect(&options(home.path()).spawn_if_missing(false))
        .await
        .unwrap_err();
    assert!(matches!(err, ConnectError::NotRunning { .. }), "{err}");
    assert!(err.to_string().contains("no daemon is running"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_older_daemon_is_asked_to_stop_and_replaced() {
    let home = tempfile::tempdir().unwrap();
    let data = data_dir(home.path());
    std::fs::create_dir_all(&data).unwrap();

    // An "older" daemon: the real router pretending to speak API v0, with
    // the token-authorised daemon.shutdown wired to end it.
    let asked = Arc::new(AtomicBool::new(false));
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let stop_tx = std::sync::Mutex::new(Some(stop_tx));
    let mut router = Router::new(RouterConfig {
        daemon_version: "0.0.1".into(),
        pid: std::process::id(),
        token: Some("old-token".into()),
    })
    .with_api_version(API_VERSION - 1);
    let flag = Arc::clone(&asked);
    router.add::<DaemonShutdown, _, _>(move |_c, p: ShutdownParams| {
        assert_eq!(p.token.as_deref(), Some("old-token"));
        flag.store(true, Ordering::Release);
        if let Some(tx) = stop_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        async { Ok(Empty {}) }
    });
    let old_endpoint = if cfg!(windows) {
        Endpoint::default_for(&data, &format!("old-{}", std::process::id()))
    } else {
        Endpoint::Path(data.join("old.sock"))
    };
    let listener = old_endpoint.listen().unwrap();
    DaemonInfo {
        pid: std::process::id(),
        endpoint: old_endpoint.clone(),
        token: "old-token".into(),
        api_version: API_VERSION - 1,
        version: "0.0.1".into(),
        started_at: "2020-01-01T00:00:00.000Z".into(),
    }
    .write(&data)
    .unwrap();
    let router = Arc::new(router);
    let data_for_old = data.clone();
    let old = tokio::spawn(async move {
        let mut stop_rx = std::pin::pin!(stop_rx);
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                accepted = listener.accept() => {
                    let (r, w) = accepted.unwrap();
                    let router = Arc::clone(&router);
                    tokio::spawn(async move { let _ = router.serve(r, w).await; });
                }
            }
        }
        // What a real daemon does on the way out.
        drop(listener);
        DaemonInfo::remove(&data_for_old).unwrap();
    });

    let connected = DaemonClient::connect(&options(home.path())).await.unwrap();
    assert!(
        asked.load(Ordering::Acquire),
        "old daemon was not asked to stop"
    );
    assert!(connected.spawned);
    assert_eq!(connected.hello.api_version, API_VERSION);
    assert_ne!(connected.info.token, "old-token");
    old.await.unwrap();

    // Without spawning, the mismatch is reported instead.
    shutdown_and_wait(&connected.client, home.path()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn incompatible_daemon_without_spawn_is_an_error() {
    let home = tempfile::tempdir().unwrap();
    let data = data_dir(home.path());
    std::fs::create_dir_all(&data).unwrap();
    let router = Arc::new(
        Router::new(RouterConfig {
            daemon_version: "9.0.0".into(),
            pid: std::process::id(),
            token: Some("t".into()),
        })
        .with_api_version(API_VERSION + 1),
    );
    let endpoint = if cfg!(windows) {
        Endpoint::default_for(&data, &format!("newer-{}", std::process::id()))
    } else {
        Endpoint::Path(data.join("newer.sock"))
    };
    let listener = endpoint.listen().unwrap();
    DaemonInfo {
        pid: std::process::id(),
        endpoint,
        token: "t".into(),
        api_version: API_VERSION + 1,
        version: "9.0.0".into(),
        started_at: "2020-01-01T00:00:00.000Z".into(),
    }
    .write(&data)
    .unwrap();
    let serve = tokio::spawn(async move {
        loop {
            let (r, w) = listener.accept().await.unwrap();
            let router = Arc::clone(&router);
            tokio::spawn(async move {
                let _ = router.serve(r, w).await;
            });
        }
    });
    // Newer daemon: never replaced, even with spawning on.
    let err = DaemonClient::connect(&options(home.path()))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            ConnectError::Incompatible { daemon_api, .. } if daemon_api == API_VERSION + 1
        ),
        "{err}"
    );
    assert!(err.to_string().contains("update this client"), "{err}");
    serve.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_daemon_exits_by_itself() {
    let home = tempfile::tempdir().unwrap();
    let mut child = spawn_daemon(home.path(), &["--idle-secs", "1"]);
    let data = data_dir(home.path());
    wait_for("daemon.json", || DaemonInfo::read(&data).unwrap().is_some());
    // A client keeps it alive...
    let client = connect_existing(home.path()).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        child.try_wait().unwrap().is_none(),
        "exited with a client connected"
    );
    client.call::<DaemonStatus>(Empty {}).await.unwrap();
    // ...and once the last one leaves, it stops.
    drop(client);
    let status = wait_exit(&mut child);
    assert!(status.success(), "{status}");
    assert!(DaemonInfo::read(&data).unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn stdio_mode_serves_one_client_without_lock_or_info_file() {
    let home = tempfile::tempdir().unwrap();
    let mut child = tokio::process::Command::new(BIN)
        .arg("--home")
        .arg(home.path())
        .arg("--stdio")
        .env_remove("HARNESS_LOG_LEVEL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let client = DaemonClient::from_streams(stdout, stdin, ClientOptions::default());
    let hello = client.hello("stdio-test", "0", None).await.unwrap();
    assert_eq!(hello.pid, child.id().unwrap());
    let status = client.call::<DaemonStatus>(Empty {}).await.unwrap();
    assert_eq!(status.sessions_open, 0);
    client
        .call::<SessionCreate>(SessionCreateParams::default())
        .await
        .unwrap();
    let data = data_dir(home.path());
    assert!(!data.join("daemon.json").exists());
    assert!(!data.join("daemon.lock").exists());

    // Closing stdin ends the daemon cleanly.
    drop(client);
    let status = tokio::time::timeout(WAIT, child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "{status}");
}

fn daemon_log(home: &Path) -> String {
    std::fs::read_dir(data_dir(home).join("logs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("daemon.")
        })
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect()
}

/// Console control events use the same `SetConsoleCtrlHandler` path as
/// closing the console; `CTRL_BREAK` is the one a test can send to another
/// process group without hitting itself.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_break_is_a_graceful_shutdown() {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(BIN)
        .arg("--home")
        .arg(home.path())
        .env_remove("HARNESS_LOG_LEVEL")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .unwrap();
    let data = data_dir(home.path());
    wait_for("daemon.json", || DaemonInfo::read(&data).unwrap().is_some());
    // Let the handler get registered before the event is raised.
    let client = connect_existing(home.path()).await;
    drop(client);

    if !console_ctrl::send_ctrl_break(child.id()) {
        eprintln!("skipped: this process has no console to raise CTRL_BREAK on");
        child.kill().unwrap();
        child.wait().unwrap();
        return;
    }
    let status = wait_exit(&mut child);
    assert!(status.success(), "{status}");
    assert!(DaemonInfo::read(&data).unwrap().is_none());
    let log = daemon_log(home.path());
    assert!(log.contains("CTRL-BREAK"), "{log}");
    assert!(log.contains("\"stopped\""), "{log}");
}

#[cfg(windows)]
mod console_ctrl {
    /// Raises `CTRL_BREAK` in the process group `pid` leads. `false` when
    /// the calling process has no console.
    #[allow(
        unsafe_code,
        reason = "GenerateConsoleCtrlEvent has no safe wrapper; a plain FFI call with no pointers"
    )]
    pub fn send_ctrl_break(pid: u32) -> bool {
        use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
        // SAFETY: both arguments are plain integers; the call touches no
        // memory owned by this process.
        unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) != 0 }
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn sigterm_is_a_graceful_shutdown() {
    let home = tempfile::tempdir().unwrap();
    let mut child = spawn_daemon(home.path(), &[]);
    let data = data_dir(home.path());
    wait_for("daemon.json", || DaemonInfo::read(&data).unwrap().is_some());
    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let status = wait_exit(&mut child);
    assert!(status.success(), "{status}");
    assert!(DaemonInfo::read(&data).unwrap().is_none());
    assert!(!data.join("daemon.sock").exists());
    let log = daemon_log(home.path());
    assert!(log.contains("SIGTERM"), "{log}");
}
