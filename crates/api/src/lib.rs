//! Wire protocol between the `harnessd` daemon and its clients (CLI, GUI).
//!
//! This crate must stay free of runtime dependencies (no `apprentice-core`) so
//! that clients never link inference or storage libraries.

/// Crate version, reported in the `daemon.hello` handshake.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Version of the JSON-RPC API surface. Bumped only on breaking changes.
pub const API_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_set() {
        assert!(!super::VERSION.is_empty());
        assert_eq!(super::API_VERSION, 1);
    }
}
