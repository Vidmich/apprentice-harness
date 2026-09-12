//! Pieces every apprentice-harness process shares: where files live and how
//! logging is set up. Deliberately tiny — the CLI and GUI link this crate but
//! must never link `apprentice-core`.

pub mod paths;
pub mod telemetry;

/// Crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
