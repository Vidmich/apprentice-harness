//! `trace.export`, `trace.import` and `trace.replay_check` over the
//! store. Each runs on the blocking pool; the daemon registers them on
//! its router next to `trace.list` / `trace.get`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::{
    TraceExport, TraceExportParams, TraceExportResult, TraceImport, TraceImportParams,
    TraceImportResult, TraceReplayCheck, TraceReplayCheckParams,
};
use apprentice_api::server::{Connection, Router};
use apprentice_api::types::{BundleSelection, ReplayReport};
use time::{OffsetDateTime, UtcOffset};

use super::export::{ExportOptions, RedactOptions, export};
use super::import::{ImportOptions, import};
use super::redact::RedactConfig;
use super::replay::{ReplaySelection, replay_check};
use crate::stats::{BoundKind, parse_bound};
use crate::trace::{TraceError, TraceStore};

/// Handlers for the three bundle methods.
#[derive(Debug, Clone)]
pub struct BundleService {
    store: Arc<TraceStore>,
    /// Holds `redact.toml`.
    config_dir: PathBuf,
    /// `None` = ask the store for the OS local offset on each request.
    offset: Option<UtcOffset>,
}

impl BundleService {
    pub fn new(store: Arc<TraceStore>, config_dir: PathBuf) -> Self {
        Self {
            store,
            config_dir,
            offset: None,
        }
    }

    /// Fixes the timezone bare dates are read in (tests).
    #[must_use]
    pub fn with_offset(mut self, offset: UtcOffset) -> Self {
        self.offset = Some(offset);
        self
    }

    fn offset(&self) -> Result<UtcOffset, TraceError> {
        match self.offset {
            Some(o) => Ok(o),
            None => Ok(
                UtcOffset::from_whole_seconds(self.store.local_offset_secs()?)
                    .unwrap_or(UtcOffset::UTC),
            ),
        }
    }

    /// # Errors
    /// `invalid_params` for an empty selection, a bad bound, an output
    /// that exists, or a pattern that does not compile; `not_found` for
    /// an unknown session; else the store's or an I/O failure.
    pub fn export(&self, p: &TraceExportParams) -> Result<TraceExportResult, RpcError> {
        let output = absolute(&p.output)?;
        let offset = self.offset()?;
        let now = OffsetDateTime::now_utc();
        let bound = |text: Option<&str>, kind| {
            text.map(|t| parse_bound(t, kind, now, offset))
                .transpose()
                .map_err(RpcError::invalid_params)
        };
        let selection = BundleSelection {
            session_ids: p.session_ids.clone(),
            workspace_id: p.workspace_id.clone(),
            since: bound(p.since.as_deref(), BoundKind::Since)?,
            until: bound(p.until.as_deref(), BoundKind::Until)?,
            all: p.all,
        };
        let redact = if p.redact || p.redact_paths {
            Some(RedactOptions {
                builtins: p.redact,
                user: if p.redact {
                    RedactConfig::load(&self.config_dir)?
                } else {
                    None
                },
                workspace_patterns: p.redact,
                paths: p.redact_paths,
                home: std::env::home_dir().map(|h| h.to_string_lossy().into_owned()),
            })
        } else {
            None
        };
        let opts = ExportOptions {
            output,
            selection,
            redact,
        };
        let (path, manifest) = export(&self.store, &opts)?;
        Ok(TraceExportResult {
            path: path.to_string_lossy().into_owned(),
            manifest,
        })
    }

    /// # Errors
    /// `not_found` for a missing bundle or workspace, `invalid_params`
    /// for one this build cannot read, `conflict` under `keep_ids`, else
    /// the verification's or the store's failure.
    pub fn import(&self, p: &TraceImportParams) -> Result<TraceImportResult, RpcError> {
        let opts = ImportOptions {
            path: absolute(&p.path)?,
            into_workspace: p.into_workspace.clone(),
            keep_ids: p.keep_ids,
        };
        let report = import(&self.store, &opts)?;
        Ok(TraceImportResult {
            sessions: report.sessions,
            counts: report.counts,
            redacted: report.redacted,
            blobs_written: report.blobs_written,
        })
    }

    /// # Errors
    /// `not_found` for an unknown call, else a store failure.
    pub fn replay_check(&self, p: &TraceReplayCheckParams) -> Result<ReplayReport, RpcError> {
        let sel = ReplaySelection {
            session_id: p.session_id.clone(),
            agent_id: p.agent_id.clone(),
            call_id: p.call_id.clone(),
        };
        Ok(replay_check(&self.store, &sel, p.rebuild)?)
    }

    pub fn register(self: Arc<Self>, router: &mut Router) {
        let svc = Arc::clone(&self);
        router.add::<TraceExport, _, _>(move |_c: Arc<Connection>, p: TraceExportParams| {
            let svc = Arc::clone(&svc);
            blocking(move || svc.export(&p))
        });
        let svc = Arc::clone(&self);
        router.add::<TraceImport, _, _>(move |_c: Arc<Connection>, p: TraceImportParams| {
            let svc = Arc::clone(&svc);
            blocking(move || svc.import(&p))
        });
        let svc = Arc::clone(&self);
        router.add::<TraceReplayCheck, _, _>(
            move |_c: Arc<Connection>, p: TraceReplayCheckParams| {
                let svc = Arc::clone(&svc);
                blocking(move || svc.replay_check(&p))
            },
        );
    }
}

/// The daemon's working directory is not the caller's: paths must be
/// absolute.
fn absolute(path: &str) -> Result<PathBuf, RpcError> {
    let p = Path::new(path);
    if path.is_empty() || !p.is_absolute() {
        return Err(RpcError::invalid_params(format!(
            "`{path}` is not an absolute path (the daemon resolves nothing relative to you)"
        )));
    }
    Ok(p.to_path_buf())
}

async fn blocking<T, F>(f: F) -> Result<T, RpcError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, RpcError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| RpcError::internal(format!("bundle task failed: {e}")))?
}
