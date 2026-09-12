//! Core of apprentice-harness: agent runtime, tool system, mentor adapter,
//! trace store, local inference service and evaluation engine.
//!
//! Hosted by the `harnessd` daemon. Clients talk to it through the
//! `apprentice-api` protocol; they never link this crate.

pub mod app;
pub mod config;
pub mod mentor;
pub mod secrets;
pub mod stats;
pub mod trace;

/// Logging setup, shared with the CLI and GUI through `apprentice-common`.
pub use apprentice_common::telemetry;

/// Crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_api_crate() {
        assert_eq!(super::VERSION, apprentice_api::VERSION);
    }
}
