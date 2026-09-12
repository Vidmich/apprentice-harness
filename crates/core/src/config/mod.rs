//! Layered configuration: built-in defaults, user `config.toml`, workspace
//! `.harness/config.toml`, environment overrides. See task M00-03.
//!
//! ```no_run
//! use apprentice_core::config::{ConfigLoader, Paths};
//!
//! let paths = Paths::discover()?;
//! let resolved = ConfigLoader::new(paths).with_process_env()?.load(None)?;
//! println!("{}", resolved.config.mentor.model);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod edit;
mod error;
mod keypath;
mod loader;
mod rpc;
mod schema;

pub use apprentice_common::paths::{APP_NAME, HOME_ENV, NoHomeDir, Paths};
pub use edit::set;
pub use error::ConfigError;
pub use loader::{ConfigLoader, Resolved, Source};
pub use rpc::{ConfigService, PROVIDERS};
pub use schema::{
    ApprenticeConfig, Config, DaemonConfig, ENV_OVERRIDES, MentorConfig, PermissionsConfig,
    Pricing, SecretStoreKind, ThinkingDisplay, TraceConfig, WORKSPACE_OVERRIDABLE, default_pricing,
    workspace_overridable,
};

/// Builds the secret store chain selected by `daemon.secret_store`.
pub fn secret_store(paths: &Paths, kind: SecretStoreKind) -> crate::secrets::ChainStore {
    use crate::secrets::{ChainStore, EnvStore, FileStore, KeychainStore, SecretStore};
    use std::sync::Arc;
    let persistent: Arc<dyn SecretStore> = match kind {
        SecretStoreKind::Keychain => Arc::new(KeychainStore::default()),
        SecretStoreKind::File => Arc::new(FileStore::new(paths.secrets_file())),
    };
    ChainStore::new(Arc::new(EnvStore::process()), persistent)
}
