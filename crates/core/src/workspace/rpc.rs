//! `workspace.*` over the registry: `add`, `list`, `remove`, `info`,
//! `refresh`. Index builds run on the blocking pool.

use std::sync::Arc;

use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::{
    Empty, WorkspaceAdd, WorkspaceAddParams, WorkspaceIdParams, WorkspaceInfo, WorkspaceInfoResult,
    WorkspaceList, WorkspaceListResult, WorkspaceRefresh, WorkspaceRemove, WorkspaceRemoveResult,
};
use apprentice_api::server::{Connection, Router};
use apprentice_api::types::{ConfigSource, WorkspaceSummary};

use super::{Workspace, Workspaces};
use crate::config::ConfigLoader;
use crate::trace::{WorkspaceId, WorkspaceRecord};

/// Handler for `workspace.*`.
#[derive(Debug)]
pub struct WorkspaceService {
    workspaces: Arc<Workspaces>,
    loader: ConfigLoader,
}

fn summary(r: &WorkspaceRecord) -> WorkspaceSummary {
    WorkspaceSummary {
        id: r.id.to_string(),
        root: r.root.clone(),
        name: r.name.clone(),
        created_at: r.created_at.clone(),
        last_used_at: r.last_used_at.clone(),
    }
}

impl WorkspaceService {
    pub fn new(workspaces: Arc<Workspaces>, loader: ConfigLoader) -> Self {
        Self { workspaces, loader }
    }

    /// `workspace.add`.
    ///
    /// # Errors
    /// Invalid params when `root` is not a directory.
    pub fn add(&self, p: &WorkspaceAddParams) -> Result<WorkspaceSummary, RpcError> {
        let root = std::path::Path::new(&p.root);
        if !root.is_absolute() {
            return Err(RpcError::invalid_params(format!(
                "workspace root must be absolute, got `{}`",
                p.root
            )));
        }
        let ws = self.workspaces.add(root, p.name.as_deref())?;
        let id = ws.id().expect("registered workspace has an id");
        Ok(summary(&self.workspaces.record(id)?))
    }

    /// `workspace.list`.
    ///
    /// # Errors
    /// Store failure.
    pub fn list(&self) -> Result<WorkspaceListResult, RpcError> {
        Ok(WorkspaceListResult {
            workspaces: self.workspaces.list()?.iter().map(summary).collect(),
        })
    }

    /// `workspace.remove`.
    ///
    /// # Errors
    /// Not found.
    pub fn remove(&self, p: &WorkspaceIdParams) -> Result<WorkspaceRemoveResult, RpcError> {
        let sessions_unlinked = self.workspaces.remove(&WorkspaceId::from(p.id.as_str()))?;
        Ok(WorkspaceRemoveResult { sessions_unlinked })
    }

    /// `workspace.info` (`refresh = false`) and `workspace.refresh`.
    ///
    /// # Errors
    /// Not found; the root is gone; the workspace config is invalid.
    pub async fn info(
        &self,
        p: &WorkspaceIdParams,
        refresh: bool,
    ) -> Result<WorkspaceInfoResult, RpcError> {
        let id = WorkspaceId::from(p.id.as_str());
        let record = self.workspaces.record(&id)?;
        let ws = self.workspaces.get(&id)?;
        let loader = self.loader.clone();
        let worker = Arc::clone(&ws);
        let (index, overrides) = tokio::task::spawn_blocking(move || {
            let index = if refresh {
                worker.refresh()
            } else {
                worker.index()
            };
            let overrides = config_overrides(&loader, &worker);
            (index, overrides)
        })
        .await
        .map_err(|e| RpcError::internal(format!("index build failed: {e}")))?;
        let overrides = overrides?;
        let head = ws.git_head();
        let git_dirty = super::git_dirty(ws.root()).await;
        Ok(WorkspaceInfoResult {
            id: record.id.to_string(),
            root: record.root,
            name: record.name,
            created_at: record.created_at,
            last_used_at: record.last_used_at,
            file_count: index.len() as u64,
            index_truncated: index.is_truncated(),
            index_age_s: index.age().as_secs(),
            git_head: head.as_ref().and_then(|h| h.commit.clone()),
            git_branch: head.and_then(|h| h.branch),
            git_dirty,
            has_instructions: ws.has_instructions(),
            has_config: ws.config_file().is_file(),
            has_ignore_file: ws.ignore_file().is_file(),
            config_overrides: overrides,
        })
    }

    pub fn register(self: Arc<Self>, router: &mut Router) {
        let svc = Arc::clone(&self);
        router.add::<WorkspaceAdd, _, _>(move |_c: Arc<Connection>, p: WorkspaceAddParams| {
            let svc = Arc::clone(&svc);
            async move { svc.add(&p) }
        });
        let svc = Arc::clone(&self);
        router.add::<WorkspaceList, _, _>(move |_c: Arc<Connection>, _p: Empty| {
            let svc = Arc::clone(&svc);
            async move { svc.list() }
        });
        let svc = Arc::clone(&self);
        router.add::<WorkspaceRemove, _, _>(move |_c: Arc<Connection>, p: WorkspaceIdParams| {
            let svc = Arc::clone(&svc);
            async move { svc.remove(&p) }
        });
        let svc = Arc::clone(&self);
        router.add::<WorkspaceInfo, _, _>(move |_c: Arc<Connection>, p: WorkspaceIdParams| {
            let svc = Arc::clone(&svc);
            async move { svc.info(&p, false).await }
        });
        let svc = Arc::clone(&self);
        router.add::<WorkspaceRefresh, _, _>(move |_c: Arc<Connection>, p: WorkspaceIdParams| {
            let svc = Arc::clone(&svc);
            async move { svc.info(&p, true).await }
        });
    }
}

/// Dotted keys the workspace's `.harness/config.toml` sets.
fn config_overrides(loader: &ConfigLoader, ws: &Workspace) -> Result<Vec<String>, RpcError> {
    let resolved = loader.load(Some(ws.root()))?;
    Ok(resolved
        .sources
        .iter()
        .filter(|(_, s)| **s == ConfigSource::Workspace)
        .map(|(k, _)| k.clone())
        .collect())
}
