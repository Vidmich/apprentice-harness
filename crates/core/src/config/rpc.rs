//! Thin `config.*` / `auth.*` RPC handlers over the loader and the secret
//! store. The daemon registers them on its router (task M00-08).

use std::sync::Arc;

use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::{
    AuthSetKey, AuthSetKeyParams, AuthStatus, AuthStatusResult, ConfigGet, ConfigGetParams,
    ConfigGetResult, ConfigPath, ConfigPathResult, ConfigSet, ConfigSetParams, Empty, ProviderAuth,
};
use apprentice_api::server::Router;

use super::edit;
use super::loader::ConfigLoader;
use crate::secrets::{ChainStore, SecretError, SecretStore, api_key_name};

/// Providers whose keys `auth.*` manages.
pub const PROVIDERS: &[&str] = &["anthropic"];

type ChangeHook = Arc<dyn Fn() + Send + Sync>;

/// Handlers for `config.get|set|path` and `auth.set_key|status`.
#[derive(Clone)]
pub struct ConfigService {
    loader: ConfigLoader,
    secrets: ChainStore,
    on_change: Option<ChangeHook>,
}

impl std::fmt::Debug for ConfigService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigService")
            .field("loader", &self.loader)
            .field("secrets", &self.secrets)
            .field("on_change", &self.on_change.is_some())
            .finish()
    }
}

impl ConfigService {
    pub fn new(loader: ConfigLoader, secrets: ChainStore) -> Self {
        Self {
            loader,
            secrets,
            on_change: None,
        }
    }

    /// Called after every successful `config.set` or `auth.set_key`, so
    /// the host can drop anything derived from the old values (the daemon
    /// rebuilds its mentor on next use).
    #[must_use]
    pub fn with_on_change(mut self, hook: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_change = Some(Arc::new(hook));
        self
    }

    fn changed(&self) {
        if let Some(hook) = &self.on_change {
            hook();
        }
    }

    pub fn loader(&self) -> &ConfigLoader {
        &self.loader
    }

    pub fn secrets(&self) -> &ChainStore {
        &self.secrets
    }

    /// # Errors
    /// Config error (-32050) or `not_found` for a missing key.
    pub fn config_get(&self, p: &ConfigGetParams) -> Result<ConfigGetResult, RpcError> {
        let resolved = self
            .loader
            .load(p.workspace.as_deref().map(std::path::Path::new))?;
        match &p.key {
            None => Ok(ConfigGetResult {
                value: resolved.tree,
                source: None,
            }),
            Some(key) => {
                let (value, source) = resolved.get(key)?;
                Ok(ConfigGetResult { value, source })
            }
        }
    }

    /// # Errors
    /// Config error (-32050).
    pub fn config_set(&self, p: ConfigSetParams) -> Result<Empty, RpcError> {
        edit::set(
            self.loader.paths(),
            p.layer,
            p.workspace.as_deref().map(std::path::Path::new),
            &p.key,
            p.value,
        )?;
        self.changed();
        Ok(Empty {})
    }

    pub fn config_path(&self) -> ConfigPathResult {
        let paths = self.loader.paths();
        ConfigPathResult {
            config_file: paths.config_file().display().to_string(),
            data_dir: paths.data_dir.display().to_string(),
        }
    }

    /// # Errors
    /// `invalid_params` for an unknown provider or empty key; config error
    /// when the store fails.
    pub fn auth_set_key(&self, p: &AuthSetKeyParams) -> Result<Empty, RpcError> {
        if !PROVIDERS.contains(&p.provider.as_str()) {
            return Err(RpcError::invalid_params(format!(
                "unknown provider `{}` (known: {})",
                p.provider,
                PROVIDERS.join(", ")
            )));
        }
        let key = p.key.trim();
        if key.is_empty() {
            return Err(RpcError::invalid_params("key must not be empty"));
        }
        self.secrets
            .set(&api_key_name(&p.provider), &key.to_owned().into())
            .map_err(|e| secret_error(&e))?;
        self.changed();
        Ok(Empty {})
    }

    /// # Errors
    /// Config error when a store fails.
    pub fn auth_status(&self) -> Result<AuthStatusResult, RpcError> {
        let mut providers = Vec::with_capacity(PROVIDERS.len());
        for name in PROVIDERS {
            let found = self
                .secrets
                .lookup(&api_key_name(name))
                .map_err(|e| secret_error(&e))?;
            providers.push(ProviderAuth {
                name: (*name).to_owned(),
                configured: found.is_some(),
                source: found.map(|(_, src)| src.to_owned()),
            });
        }
        Ok(AuthStatusResult { providers })
    }

    /// Registers all five methods on `router`. File and keychain access is
    /// blocking, so each handler runs on the blocking pool.
    pub fn register(self: Arc<Self>, router: &mut Router) {
        use apprentice_api::server::Connection;

        let svc = Arc::clone(&self);
        router.add::<ConfigGet, _, _>(move |_c: Arc<Connection>, p: ConfigGetParams| {
            let svc = Arc::clone(&svc);
            blocking(move || svc.config_get(&p))
        });
        let svc = Arc::clone(&self);
        router.add::<ConfigSet, _, _>(move |_c: Arc<Connection>, p: ConfigSetParams| {
            let svc = Arc::clone(&svc);
            blocking(move || svc.config_set(p))
        });
        let svc = Arc::clone(&self);
        router.add::<ConfigPath, _, _>(move |_c: Arc<Connection>, Empty {}| {
            let r = svc.config_path();
            async move { Ok(r) }
        });
        let svc = Arc::clone(&self);
        router.add::<AuthSetKey, _, _>(move |_c: Arc<Connection>, p: AuthSetKeyParams| {
            let svc = Arc::clone(&svc);
            blocking(move || svc.auth_set_key(&p))
        });
        let svc = Arc::clone(&self);
        router.add::<AuthStatus, _, _>(move |_c: Arc<Connection>, Empty {}| {
            let svc = Arc::clone(&svc);
            blocking(move || svc.auth_status())
        });
    }
}

async fn blocking<T, F>(f: F) -> Result<T, RpcError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, RpcError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| RpcError::internal(format!("config task failed: {e}")))?
}

fn secret_error(e: &SecretError) -> RpcError {
    let reason = match e {
        SecretError::Keychain(_) => "keychain",
        SecretError::Io { .. } => "io",
        SecretError::Syntax { .. } => "syntax",
        SecretError::ReadOnly(_) => "read_only",
    };
    RpcError::config(format!("secret store: {e}"))
        .with_details(serde_json::json!({ "reason": reason }))
}
