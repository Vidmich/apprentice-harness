//! Client library used by the CLI and the GUI backend to find, spawn and talk
//! to the `harnessd` daemon.

/// Crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_api_crate() {
        assert_eq!(super::VERSION, apprentice_api::VERSION);
    }
}
