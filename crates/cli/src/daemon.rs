//! Connecting to the daemon by discovery. Spawning one when none runs
//! (with `--no-spawn` to opt out) is wired by M00-09; until then a missing
//! daemon is an error with a hint.

use apprentice_client::{ConnectOptions, DaemonClient};
use apprentice_common::paths::Paths;

/// Runs `f` against the daemon named by `daemon.json` on a fresh
/// current-thread runtime.
///
/// # Errors
/// No daemon (or an unreadable `daemon.json`), a failed handshake, or
/// whatever `f` returns.
pub fn with_client<T, F, Fut>(paths: &Paths, f: F) -> anyhow::Result<T>
where
    F: FnOnce(DaemonClient) -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let options = ConnectOptions::new(&paths.data_dir)
        .spawn_if_missing(false)
        .client_name("harness-cli", apprentice_client::VERSION);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let connected = DaemonClient::connect(&options).await?;
        f(connected.client).await
    })
}
