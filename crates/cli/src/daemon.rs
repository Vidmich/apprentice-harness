//! Connecting to a running daemon by discovery. Auto-starting the daemon
//! when none is running arrives with M00-08/M00-09.

use std::time::Duration;

use anyhow::{Context, bail};
use apprentice_client::{ClientOptions, DaemonClient, DaemonInfo};
use apprentice_common::paths::Paths;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Runs `f` against the daemon named by `daemon.json` on a fresh
/// current-thread runtime.
///
/// # Errors
/// No daemon (or an unreadable `daemon.json`), a connection failure, or
/// whatever `f` returns.
pub fn with_client<T, F, Fut>(paths: &Paths, f: F) -> anyhow::Result<T>
where
    F: FnOnce(DaemonClient) -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let Some(info) = DaemonInfo::read(&paths.data_dir)? else {
        bail!(
            "no daemon is running ({} not found); start one with `harness daemon start`",
            DaemonInfo::path(&paths.data_dir).display()
        );
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("cannot start async runtime")?;
    rt.block_on(async {
        let client = tokio::time::timeout(
            CONNECT_TIMEOUT,
            DaemonClient::connect_endpoint(&info.endpoint, ClientOptions::default()),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out connecting to {}", info.endpoint))?
        .with_context(|| format!("cannot connect to daemon at {}", info.endpoint))?;
        client
            .hello("harness-cli", apprentice_client::VERSION, Some(info.token))
            .await
            .context("daemon handshake failed")?;
        f(client).await
    })
}
