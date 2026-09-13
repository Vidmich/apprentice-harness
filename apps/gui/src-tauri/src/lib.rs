//! Tauri backend of the apprentice-harness GUI (task M00-10).
//!
//! A thin client of the daemon, exactly like the CLI: the connection
//! manager in [`daemon`] finds or spawns `harnessd` through
//! `apprentice-client`, and [`bridge`] passes RPC calls and event streams
//! through to the frontend untyped. This crate must never depend on
//! `apprentice-core`.

pub mod bridge;
pub mod daemon;
pub mod host;

use apprentice_client::ConnectOptions;
use apprentice_common::paths::Paths;
use apprentice_common::telemetry;
use serde::Serialize;

/// Application version, reported to the frontend.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Static facts about this app instance.
#[derive(Debug, Clone, Serialize)]
pub struct AppInfo {
    pub version: String,
    pub config_file: String,
    pub data_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_file: Option<String>,
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value, reason = "tauri command signature")]
fn app_info(info: tauri::State<'_, AppInfo>) -> AppInfo {
    info.inner().clone()
}

/// Connect options for this app: spawn the daemon when none runs, let it
/// discover its paths the way we did (`HARNESS_HOME` is inherited).
fn connect_options(paths: &Paths) -> ConnectOptions {
    ConnectOptions::new(&paths.data_dir).client_name("harness-gui", VERSION)
}

/// Builds and runs the Tauri application.
///
/// # Panics
/// Panics if the Tauri runtime fails to start or no home directory can be
/// determined for configuration and data.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let paths = Paths::discover().expect("cannot determine the configuration directory");
    let level = telemetry::resolve_level(None, telemetry::level_from_env().as_deref(), None);
    // Logging is best effort: a read-only data dir must not stop the app.
    let log = telemetry::init(
        &telemetry::Options::new("gui", level, paths.data_dir.join("logs"))
            .stderr(cfg!(debug_assertions)),
    )
    .map_err(|e| eprintln!("warning: logging disabled: {e}"))
    .ok();
    telemetry::install_panic_hook();

    let info = AppInfo {
        version: VERSION.to_owned(),
        config_file: paths.config_file().display().to_string(),
        data_dir: paths.data_dir.display().to_string(),
        log_file: log.map(|l| l.log_file.display().to_string()),
    };
    let state = daemon::DaemonState::new(connect_options(&paths));

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(info)
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            app_info,
            daemon::daemon_status,
            daemon::daemon_restart,
            bridge::rpc_call,
            bridge::rpc_stream,
            host::open_path,
            host::write_text_file,
            host::dir_size,
            host::quit_and_stop_daemon,
        ])
        .setup(|app| {
            daemon::spawn_manager(app.handle().clone());
            if let Err(e) = host::build_tray(app.handle()) {
                eprintln!("warning: no tray icon: {e}");
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_client_crate() {
        assert_eq!(VERSION, apprentice_client::VERSION);
    }

    #[test]
    fn the_gui_spawns_the_daemon_and_names_itself() {
        let options = connect_options(&Paths::from_home("/tmp/h"));
        assert!(options.spawn_if_missing);
        assert_eq!(options.client_name, "harness-gui");
        assert!(options.home.is_none(), "the daemon discovers its own paths");
    }
}
