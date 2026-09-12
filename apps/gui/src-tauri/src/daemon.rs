//! The connection to `harnessd`: a background task that connects (spawning
//! the daemon when none runs), watches the connection and reconnects with
//! backoff, publishing every change as a `daemon:status` event. Commands
//! borrow the current [`DaemonClient`] through [`DaemonState::client`].

use std::sync::Mutex;
use std::time::Duration;

use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::{DaemonShutdown, ShutdownParams};
use apprentice_client::{ConnectOptions, DaemonClient};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;
use tracing::{info, warn};

/// Tauri event carrying a [`DaemonStatus`] on every change.
pub const STATUS_EVENT: &str = "daemon:status";

const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(5);

/// What the frontend knows about the daemon connection.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DaemonStatus {
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Why the last connection attempt failed, or why it ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The daemon was started by this app (as opposed to already running).
    pub spawned: bool,
}

/// Managed state: the live client plus the last published status.
#[derive(Debug)]
pub struct DaemonState {
    options: ConnectOptions,
    client: Mutex<Option<DaemonClient>>,
    status: Mutex<DaemonStatus>,
    /// Wakes the manager so it retries at once (after `daemon_restart`).
    kick: Notify,
}

impl DaemonState {
    pub fn new(options: ConnectOptions) -> Self {
        Self {
            options,
            client: Mutex::new(None),
            status: Mutex::new(DaemonStatus::default()),
            kick: Notify::new(),
        }
    }

    /// The current connection, or the error the frontend shows when there
    /// is none.
    pub fn client(&self) -> Result<DaemonClient, RpcError> {
        self.client
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .filter(|c| !c.is_closed())
            .ok_or_else(|| {
                let status = self.status();
                RpcError::new(
                    apprentice_api::jsonrpc::codes::INTERNAL_ERROR,
                    "daemon_unavailable",
                    match status.error {
                        Some(e) => format!("daemon unavailable: {e}"),
                        None => "daemon unavailable".to_owned(),
                    },
                )
            })
    }

    pub fn status(&self) -> DaemonStatus {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set_client(&self, client: Option<DaemonClient>) {
        *self
            .client
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = client;
    }

    fn publish(&self, app: &AppHandle, status: &DaemonStatus) {
        *self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = status.clone();
        if let Err(e) = app.emit(STATUS_EVENT, status) {
            warn!(error = %e, "cannot emit daemon status");
        }
    }
}

/// Runs the connection manager for the life of the app.
pub fn spawn_manager(app: AppHandle) {
    tauri::async_runtime::spawn(async move { manage(&app).await });
}

async fn manage(app: &AppHandle) {
    let state = app.state::<DaemonState>();
    let mut backoff = BACKOFF_MIN;
    loop {
        match DaemonClient::connect(&state.options).await {
            Ok(connected) => {
                backoff = BACKOFF_MIN;
                info!(
                    pid = connected.hello.pid,
                    version = connected.hello.daemon_version,
                    spawned = connected.spawned,
                    "connected to daemon"
                );
                state.set_client(Some(connected.client.clone()));
                state.publish(
                    app,
                    &DaemonStatus {
                        connected: true,
                        version: Some(connected.hello.daemon_version),
                        pid: Some(connected.hello.pid),
                        error: None,
                        spawned: connected.spawned,
                    },
                );
                // `kick` (a restart request) is also how the app learns
                // the daemon is going away on purpose; either way we come
                // back around and reconnect, respawning if needed.
                tokio::select! {
                    () = connected.client.closed() => {}
                    () = state.kick.notified() => {}
                }
                state.set_client(None);
                warn!("daemon connection lost");
                state.publish(
                    app,
                    &DaemonStatus {
                        connected: false,
                        error: Some("connection to daemon closed".into()),
                        ..DaemonStatus::default()
                    },
                );
                // A daemon that is stopping on purpose needs a moment to
                // release its lock; reconnecting at once would spawn a
                // replacement that exits with "already running" and leave
                // us waiting out the whole spawn timeout.
                tokio::time::sleep(BACKOFF_MIN).await;
            }
            Err(e) => {
                warn!(error = %e, "cannot reach the daemon");
                state.publish(
                    app,
                    &DaemonStatus {
                        connected: false,
                        error: Some(e.to_string()),
                        ..DaemonStatus::default()
                    },
                );
                tokio::select! {
                    () = tokio::time::sleep(backoff) => {}
                    () = state.kick.notified() => {}
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

/// Last published status, so a freshly mounted frontend does not have to
/// wait for the next change.
#[tauri::command]
#[allow(clippy::needless_pass_by_value, reason = "tauri command signature")]
pub fn daemon_status(state: tauri::State<'_, DaemonState>) -> DaemonStatus {
    state.status()
}

/// Asks the daemon to stop; the manager reconnects (spawning a new one)
/// as soon as the connection drops. With no connection it just retries.
#[tauri::command]
pub async fn daemon_restart(state: tauri::State<'_, DaemonState>) -> Result<(), RpcError> {
    match state.client() {
        Ok(client) => {
            client
                .call::<DaemonShutdown>(ShutdownParams {
                    graceful: true,
                    token: None,
                })
                .await
                .map_err(crate::bridge::client_error)?;
        }
        Err(_) => state.kick.notify_one(),
    }
    Ok(())
}
