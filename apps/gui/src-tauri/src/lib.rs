//! Tauri backend of the apprentice-harness GUI.
//!
//! M00-01: opens an empty window. The daemon connection manager and the RPC
//! bridge commands arrive with M00-10. This crate must never depend on
//! `apprentice-core`.

/// Application version, reported to the frontend.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Builds and runs the Tauri application.
///
/// # Panics
/// Panics if the Tauri runtime fails to start.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_client_crate() {
        assert_eq!(super::VERSION, apprentice_client::VERSION);
    }
}
