//! What the app does on the host besides talking to the daemon (task
//! M01-12): opening a file or folder with the OS default, writing an
//! export where the user chose, sizing the data directory, quitting
//! with the daemon stopped, and the tray icon. Closing the window on
//! its own leaves the daemon running (it was spawned detached).

use std::path::{Path, PathBuf};

use apprentice_api::jsonrpc::{RpcError, codes};
use apprentice_api::methods::{DaemonShutdown, ShutdownParams};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt as _;
use tracing::warn;

use crate::daemon::DaemonState;

fn host_error(message: impl Into<String>) -> RpcError {
    RpcError::new(codes::INTERNAL_ERROR, "host", message)
}

/// Opens `path` with the OS default application (a rules file in the
/// editor, the data folder in the file manager).
#[tauri::command]
#[allow(clippy::needless_pass_by_value, reason = "tauri command signature")]
pub fn open_path(app: AppHandle, path: String) -> Result<(), RpcError> {
    app.opener()
        .open_path(&path, None::<&str>)
        .map_err(|e| host_error(format!("cannot open {path}: {e}")))
}

/// Writes `contents` to `path` (the target of a save dialog).
#[tauri::command]
#[allow(clippy::needless_pass_by_value, reason = "tauri command signature")]
pub fn write_text_file(path: String, contents: String) -> Result<(), RpcError> {
    std::fs::write(&path, contents).map_err(|e| host_error(format!("cannot write {path}: {e}")))
}

/// Bytes of every file under `path`, symlinks not followed.
#[tauri::command]
pub async fn dir_size(path: String) -> Result<u64, RpcError> {
    let root = PathBuf::from(&path);
    tauri::async_runtime::spawn_blocking(move || size_of(&root))
        .await
        .map_err(|e| host_error(format!("cannot size {path}: {e}")))
}

/// The recursive size; unreadable entries count as nothing.
pub fn size_of(root: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}

/// Stops the daemon (when connected) and exits the app.
#[tauri::command]
pub async fn quit_and_stop_daemon(app: AppHandle) -> Result<(), RpcError> {
    stop_daemon(&app).await;
    app.exit(0);
    Ok(())
}

async fn stop_daemon(app: &AppHandle) {
    let state = app.state::<DaemonState>();
    if let Ok(client) = state.client()
        && let Err(e) = client
            .call::<DaemonShutdown>(ShutdownParams {
                graceful: true,
                token: None,
            })
            .await
    {
        warn!(error = %e, "daemon.shutdown failed; quitting anyway");
    }
}

/// The tray icon: show the window, quit, or quit with the daemon.
///
/// # Errors
/// The menu or the icon cannot be created.
pub fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show apprentice-harness", true, None::<&str>)?;
    let quit = MenuItem::with_id(
        app,
        "quit",
        "Quit (daemon keeps running)",
        true,
        None::<&str>,
    )?;
    let stop = MenuItem::with_id(
        app,
        "quit_stop",
        "Quit and stop the daemon",
        true,
        None::<&str>,
    )?;
    let menu = Menu::with_items(app, &[&show, &quit, &stop])?;
    let mut tray = TrayIconBuilder::with_id("main")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("apprentice-harness")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_window(app),
            "quit" => app.exit(0),
            "quit_stop" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    stop_daemon(&app).await;
                    app.exit(0);
                });
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let tauri::tray::TrayIconEvent::DoubleClick { .. } = event {
                show_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_size_sums_files_recursively() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "12345").unwrap();
        std::fs::create_dir_all(dir.path().join("sub/deep")).unwrap();
        std::fs::write(dir.path().join("sub/b.txt"), "123").unwrap();
        std::fs::write(dir.path().join("sub/deep/c.txt"), "12").unwrap();
        assert_eq!(size_of(dir.path()), 10);
        assert_eq!(size_of(&dir.path().join("missing")), 0);
    }
}
