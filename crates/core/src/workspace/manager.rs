//! The registry: `workspaces` rows in the trace store plus one open
//! [`Workspace`] per id, shared by every session on the same root.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use super::{Workspace, WorkspaceError, paths};
use crate::trace::{TraceError, TraceStore, WorkspaceId, WorkspaceRecord};

/// Registered workspaces and their open handles. One per process.
pub struct Workspaces {
    store: Arc<TraceStore>,
    open: Mutex<HashMap<WorkspaceId, Arc<Workspace>>>,
}

impl std::fmt::Debug for Workspaces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspaces")
            .field("open", &self.open_count())
            .finish_non_exhaustive()
    }
}

impl Workspaces {
    pub fn new(store: Arc<TraceStore>) -> Self {
        Self {
            store,
            open: Mutex::new(HashMap::new()),
        }
    }

    /// Handles currently open.
    pub fn open_count(&self) -> usize {
        self.open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Registers `root` (or finds it: the same root always yields the
    /// same id) and returns its open handle. `name` only applies to a
    /// new row.
    ///
    /// # Errors
    /// `root` is not an existing directory; store failure.
    pub fn add(&self, root: &Path, name: Option<&str>) -> Result<Arc<Workspace>, WorkspaceError> {
        let canonical = paths::canonical_dir(root).map_err(|source| WorkspaceError::Root {
            path: root.to_path_buf(),
            source,
        })?;
        let record = self
            .store
            .add_workspace(&canonical.to_string_lossy(), name)?;
        self.open_record(&record)
    }

    /// The open handle for a registered id (opened on first use).
    ///
    /// # Errors
    /// Unknown id, or the stored root is gone.
    pub fn get(&self, id: &WorkspaceId) -> Result<Arc<Workspace>, WorkspaceError> {
        if let Some(ws) = self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
        {
            return Ok(Arc::clone(ws));
        }
        let record = self.store.get_workspace(id)?;
        self.open_record(&record)
    }

    /// The registry row for an id.
    ///
    /// # Errors
    /// Unknown id.
    pub fn record(&self, id: &WorkspaceId) -> Result<WorkspaceRecord, TraceError> {
        self.store.get_workspace(id)
    }

    /// A handle for a root that may or may not be registered: the
    /// registered one when it is, an ad-hoc `Workspace` otherwise.
    ///
    /// # Errors
    /// `root` is not an existing directory.
    pub fn open_root(&self, root: &Path) -> Result<Arc<Workspace>, WorkspaceError> {
        let ws = Workspace::open(root)?;
        match self.store.find_workspace(&ws.root_string())? {
            Some(record) => self.open_record(&record),
            None => Ok(Arc::new(ws)),
        }
    }

    /// Every registered workspace, most recently used first.
    ///
    /// # Errors
    /// Store failure.
    pub fn list(&self) -> Result<Vec<WorkspaceRecord>, TraceError> {
        self.store.list_workspaces()
    }

    /// Forgets a workspace and drops its handle. Returns the number of
    /// sessions unlinked.
    ///
    /// # Errors
    /// Unknown id.
    pub fn remove(&self, id: &WorkspaceId) -> Result<u64, WorkspaceError> {
        let n = self.store.remove_workspace(id)?;
        self.open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
        Ok(n)
    }

    /// Marks a workspace used now.
    ///
    /// # Errors
    /// Unknown id.
    pub fn touch(&self, id: &WorkspaceId) -> Result<(), TraceError> {
        self.store.touch_workspace(id)
    }

    fn open_record(&self, record: &WorkspaceRecord) -> Result<Arc<Workspace>, WorkspaceError> {
        let mut open = self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(ws) = open.get(&record.id) {
            return Ok(Arc::clone(ws));
        }
        let ws = Arc::new(Workspace::from_record(record)?);
        open.insert(record.id.clone(), Arc::clone(&ws));
        Ok(ws)
    }
}
