//! Wire protocol between the `harnessd` daemon and its clients (CLI, GUI).
//!
//! - [`jsonrpc`]: JSON-RPC 2.0 message types and the error model
//! - [`methods`]: the typed method set ([`methods::Method`])
//! - [`events`]: daemon → client notifications
//! - [`types`]: data types shared by params, results and events
//! - [`codec`]: newline-delimited JSON framing
//! - [`server`]: the request router the daemon runs per connection
//! - [`transport`]: local socket endpoints (named pipe / Unix socket)
//!
//! This crate must stay free of runtime dependencies (no `apprentice-core`) so
//! that clients never link inference or storage libraries.

pub mod codec;
pub mod events;
pub mod jsonrpc;
pub mod methods;
pub mod server;
pub mod transport;
pub mod types;

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
