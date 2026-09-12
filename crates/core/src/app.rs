//! State shared by everything one daemon process hosts (task M00-08):
//! paths, the config loader, the trace store and its writer, the secret
//! store, the lazily built mentor, the agent registry and the shutdown
//! token. RPC handlers are thin adapters over it; [`AppState::register`]
//! wires all of them.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::{SessionCreate, SessionCreateParams, SessionCreateResult};
use apprentice_api::server::{Connection, Router};
use tokio_util::sync::CancellationToken;

use crate::config::{Config, ConfigError, ConfigLoader, ConfigService, Paths, secret_store};
use crate::mentor::{AnthropicMentor, Mentor};
use crate::runtime::AgentRegistry;
use crate::secrets::{ChainStore, SecretError, api_key_name};
use crate::stats::StatsService;
use crate::trace::{NewSession, SessionStatus, TraceError, TraceService, TraceStore, TraceWriter};

/// Errors from opening the state or building the mentor.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Trace(#[from] TraceError),
    #[error("secret store: {0}")]
    Secret(#[from] SecretError),
    /// No API key for `provider` anywhere in the secret chain.
    #[error("no API key for {provider}; run `harness auth set-key`")]
    NoApiKey { provider: String },
    #[error("cannot build HTTP client: {0}")]
    Http(#[from] reqwest::Error),
}

impl From<AppError> for RpcError {
    fn from(e: AppError) -> Self {
        match e {
            AppError::Config(e) => e.into(),
            AppError::Trace(e) => e.into(),
            AppError::Secret(e) => RpcError::config(format!("secret store: {e}")),
            AppError::NoApiKey { .. } => RpcError::config(e.to_string())
                .with_details(serde_json::json!({ "reason": "no_api_key" })),
            AppError::Http(e) => RpcError::internal(e.to_string()),
        }
    }
}

/// Everything the handlers share. One per process, behind an `Arc`.
pub struct AppState {
    loader: ConfigLoader,
    store: Arc<TraceStore>,
    writer: TraceWriter,
    secrets: ChainStore,
    mentor: Mutex<Option<Arc<dyn Mentor>>>,
    agents: AgentRegistry,
    shutdown: CancellationToken,
}

/// How long [`AppState::close`] waits for cancelled agents to record
/// their end. Under the daemon's hard deadline (10 s) with its drain of
/// connections (5 s) accounted for.
pub const AGENT_DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("paths", self.paths())
            .field("store", &self.store)
            .field(
                "mentor_built",
                &self.mentor.lock().is_ok_and(|m| m.is_some()),
            )
            .field("agents_running", &self.agents.running())
            .field("shutting_down", &self.shutdown.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl AppState {
    /// Loads the config for `paths` and opens everything.
    ///
    /// # Errors
    /// Invalid config, or the trace store cannot be opened.
    pub fn open(paths: Paths) -> Result<Arc<Self>, AppError> {
        let loader = ConfigLoader::new(paths).with_process_env()?;
        let config = loader.load(None)?.config;
        Self::open_with(loader, &config)
    }

    /// Opens the trace store and secret store as `config` says. The caller
    /// picks `config` so a daemon can still come up on defaults when the
    /// user's file is broken (and be fixed through `config.set`).
    ///
    /// # Errors
    /// The trace store cannot be opened.
    pub fn open_with(loader: ConfigLoader, config: &Config) -> Result<Arc<Self>, AppError> {
        let paths = loader.paths();
        let store = Arc::new(TraceStore::open_with(paths, &config.trace)?);
        let writer = TraceWriter::spawn(Arc::clone(&store));
        let secrets = secret_store(paths, config.daemon.secret_store);
        Ok(Arc::new(Self {
            loader,
            store,
            writer,
            secrets,
            mentor: Mutex::new(None),
            agents: AgentRegistry::new(),
            shutdown: CancellationToken::new(),
        }))
    }

    pub fn paths(&self) -> &Paths {
        self.loader.paths()
    }

    pub fn loader(&self) -> &ConfigLoader {
        &self.loader
    }

    pub fn store(&self) -> &Arc<TraceStore> {
        &self.store
    }

    pub fn writer(&self) -> &TraceWriter {
        &self.writer
    }

    pub fn secrets(&self) -> &ChainStore {
        &self.secrets
    }

    /// Cancelled when the daemon shuts down; every agent's token is a
    /// child of it.
    pub fn shutdown(&self) -> &CancellationToken {
        &self.shutdown
    }

    /// The agents running in this process.
    pub fn agents(&self) -> &AgentRegistry {
        &self.agents
    }

    /// The current user-level config, read fresh (the loader is cheap and
    /// `config.set` edits the file behind our back by design).
    ///
    /// # Errors
    /// Invalid config.
    pub fn config(&self) -> Result<Config, ConfigError> {
        Ok(self.loader.load(None)?.config)
    }

    /// The mentor, built on first use from `[mentor]` and the provider key
    /// so a missing key does not stop the daemon from starting. Dropped by
    /// [`Self::invalidate_mentor`] after config or key changes.
    ///
    /// # Errors
    /// Invalid config, no key, or no HTTP client.
    pub fn mentor(&self) -> Result<Arc<dyn Mentor>, AppError> {
        let mut slot = self
            .mentor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(m) = &*slot {
            return Ok(Arc::clone(m));
        }
        let config = self.config()?;
        let provider = "anthropic";
        let (key, _) = self
            .secrets
            .lookup(&api_key_name(provider))?
            .ok_or_else(|| AppError::NoApiKey {
                provider: provider.to_owned(),
            })?;
        let http = AnthropicMentor::http_client(&config.mentor)?;
        let mentor: Arc<dyn Mentor> = Arc::new(
            AnthropicMentor::new(&config.mentor, key, http)
                .with_raw_sse(config.trace.capture_raw_sse),
        );
        *slot = Some(Arc::clone(&mentor));
        Ok(mentor)
    }

    /// Forgets the built mentor; the next [`Self::mentor`] rebuilds it.
    pub fn invalidate_mentor(&self) {
        self.mentor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }

    /// Sessions with status `open`.
    ///
    /// # Errors
    /// Store failure.
    pub fn sessions_open(&self) -> Result<u64, TraceError> {
        self.store.count_sessions(SessionStatus::Open)
    }

    /// `session.create`: a new session carrying a snapshot of the config
    /// resolved for its workspace.
    ///
    /// # Errors
    /// Invalid config for that workspace, or store failure.
    pub async fn session_create(
        &self,
        p: &SessionCreateParams,
    ) -> Result<SessionCreateResult, RpcError> {
        let workspace = p.workspace.as_deref().map(std::path::Path::new);
        let config = self.loader.load(workspace)?.tree;
        let session = NewSession {
            title: p.title.clone(),
            workspace_path: p.workspace.clone(),
            config,
        };
        let id = self
            .writer
            .run(move |store| store.create_session(&session))
            .await?;
        Ok(SessionCreateResult {
            session_id: id.into_string(),
        })
    }

    /// Registers every core handler: `config.*`, `auth.*`, `session.*`,
    /// `agent.*`, `trace.*`, `stats.*`. `daemon.*` is the host's.
    pub fn register(self: &Arc<Self>, router: &mut Router) {
        crate::runtime::rpc::register(self, router);
        let state = Arc::clone(self);
        let config = ConfigService::new(self.loader.clone(), self.secrets.clone())
            .with_on_change(move || state.invalidate_mentor());
        Arc::new(config).register(router);
        Arc::new(TraceService::new(Arc::clone(&self.store))).register(router);
        Arc::new(StatsService::new(
            Arc::clone(&self.store),
            self.loader.clone(),
        ))
        .register(router);
        let state = Arc::clone(self);
        router.add::<SessionCreate, _, _>(move |_c: Arc<Connection>, p: SessionCreateParams| {
            let state = Arc::clone(&state);
            async move { state.session_create(&p).await }
        });
    }

    /// Cancels the agents, waits for them to record their end, then
    /// flushes and stops the trace writer. Call once, at shutdown.
    pub async fn close(&self) {
        self.shutdown.cancel();
        if !self.agents.drain(AGENT_DRAIN_TIMEOUT).await {
            tracing::warn!(
                running = self.agents.running(),
                "agents still running at shutdown; their end is not recorded"
            );
        }
        if let Err(e) = self.writer.flush().await {
            tracing::warn!(error = %e, "trace flush failed at shutdown");
        }
        self.writer.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SecretStoreKind;
    use crate::secrets::SecretStore as _;

    fn state(dir: &std::path::Path) -> Arc<AppState> {
        let paths = Paths::from_home(dir);
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        let loader = ConfigLoader::new(paths);
        let mut config = Config::default();
        config.daemon.secret_store = SecretStoreKind::File;
        AppState::open_with(loader, &config).unwrap()
    }

    #[tokio::test]
    async fn sessions_are_created_with_a_config_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path());
        assert_eq!(state.sessions_open().unwrap(), 0);
        let r = state
            .session_create(&SessionCreateParams {
                workspace: None,
                title: Some("t".into()),
            })
            .await
            .unwrap();
        let record = state.store().get_session(&r.session_id.into()).unwrap();
        assert_eq!(record.title.as_deref(), Some("t"));
        assert_eq!(state.sessions_open().unwrap(), 1);
        state.close().await;
    }

    #[tokio::test]
    async fn mentor_needs_a_key_and_is_rebuilt_after_changes() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path());
        let Err(err) = state.mentor() else {
            panic!("mentor built without a key")
        };
        assert!(matches!(err, AppError::NoApiKey { .. }), "{err}");

        state
            .secrets()
            .set(&api_key_name("anthropic"), &"sk-test".to_owned().into())
            .unwrap();
        let a = state.mentor().unwrap();
        let b = state.mentor().unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        state.invalidate_mentor();
        let c = state.mentor().unwrap();
        assert!(!Arc::ptr_eq(&a, &c));

        // The hook wired by `register` drops it on config.set.
        let mut router = Router::new(apprentice_api::server::RouterConfig {
            daemon_version: "0".into(),
            pid: 1,
            token: None,
        });
        state.register(&mut router);
        let mut names = router.methods();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "agent.cancel",
                "agent.run",
                "agent.subscribe",
                "auth.set_key",
                "auth.status",
                "config.get",
                "config.path",
                "config.set",
                "session.create",
                "session.list",
                "stats.reprice",
                "stats.tokens",
                "trace.get",
                "trace.list",
            ]
        );
        state.close().await;
    }
}
