//! Core of apprentice-harness: agent runtime, tool system, mentor adapter,
//! trace store, local inference service and evaluation engine.
//!
//! Hosted by the `harnessd` daemon. Clients talk to it through the
//! `apprentice-api` protocol; they never link this crate.

pub mod config;
pub mod secrets;

/// Crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_api_crate() {
        assert_eq!(super::VERSION, apprentice_api::VERSION);
    }
}
