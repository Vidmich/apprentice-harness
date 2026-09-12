//! Workspaces (task M01-02): a registered root directory, usually a
//! repository, that sessions attach to.
//!
//! A [`Workspace`] gives tools their path sandbox ([`Workspace::resolve`],
//! [`Workspace::contains`], [`Workspace::display`]), the ignore rules for
//! discovery ([`Workspace::walker`]), the `.harness/` conventions and a
//! cheap [`FileIndex`] built on first use. [`Workspaces`] is the
//! registry: rows in `traces.sqlite` (schema v2) plus the open
//! `Workspace` per id, so the index is shared by every session on the
//! same root. [`WorkspaceService`] exposes `workspace.*` over RPC.
//!
//! The rules in one place:
//! - every path a tool touches goes through `resolve`; what lands
//!   outside the root is [`PathError::Outside`], which the permission
//!   engine (M01-07) may turn into a question for the user;
//! - ignore rules are for discovery only, never access control;
//! - paths shown to the mentor are root-relative with `/` on every OS.

mod git;
mod index;
mod manager;
mod paths;
pub(crate) mod rpc;
mod rules;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

pub use git::{GitHead, git_head};
pub use index::{FileEntry, FileIndex, INDEX_TTL, MAX_INDEX_FILES, language_of};
pub use manager::Workspaces;
pub use paths::PathError;
pub use rpc::WorkspaceService;
pub use rules::{BUILTIN_IGNORES, IgnoreRules};

use crate::trace::{TraceError, WorkspaceId, WorkspaceRecord};

/// Per-workspace directory under the root.
pub const HARNESS_DIR: &str = ".harness";
/// Project instructions, injected into the system prompt (M01-09).
pub const INSTRUCTIONS_FILE: &str = "HARNESS.md";
/// Extra ignore patterns, gitignore syntax.
pub const IGNORE_FILE: &str = "ignore";
/// Workspace config layer (M00-03).
pub const CONFIG_FILE: &str = "config.toml";
/// Per-workspace permission rules (M01-07).
pub const PERMISSIONS_FILE: &str = "permissions.toml";

/// Errors of the registry and of opening a root.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// The root does not exist, is not a directory, or cannot be
    /// canonicalised.
    #[error("workspace root `{path}`: {source}")]
    Root {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Path(#[from] PathError),
    #[error(transparent)]
    Trace(#[from] TraceError),
}

impl WorkspaceError {
    /// Stable machine-readable kind.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Root { .. } => "bad_root",
            Self::Path(e) => e.kind(),
            Self::Trace(e) => e.kind(),
        }
    }
}

impl From<WorkspaceError> for apprentice_api::jsonrpc::RpcError {
    fn from(e: WorkspaceError) -> Self {
        use apprentice_api::jsonrpc::RpcError;
        match e {
            WorkspaceError::Root { .. } => RpcError::invalid_params(e.to_string()),
            WorkspaceError::Path(p) => RpcError::invalid_params(p.to_string())
                .with_details(serde_json::json!({ "reason": p.kind() })),
            WorkspaceError::Trace(t) => t.into(),
        }
    }
}

/// An open workspace root. Cheap to share (`Arc`); the index is built
/// lazily and swapped atomically.
pub struct Workspace {
    id: Option<WorkspaceId>,
    root: PathBuf,
    name: String,
    rules: RwLock<IgnoreRules>,
    index: Mutex<Option<Arc<FileIndex>>>,
}

impl std::fmt::Debug for Workspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspace")
            .field("id", &self.id)
            .field("root", &self.root)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Workspace {
    /// Opens `root` without registering it (ad-hoc sessions, tests).
    /// The root is canonicalised, so symlinked roots resolve to their
    /// target.
    ///
    /// # Errors
    /// `root` is not an existing directory.
    pub fn open(root: &Path) -> Result<Self, WorkspaceError> {
        let root = paths::canonical_dir(root).map_err(|source| WorkspaceError::Root {
            path: root.to_path_buf(),
            source,
        })?;
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| root.to_string_lossy().into_owned());
        Ok(Self::at(root, name, None))
    }

    /// Opens the root of a registry row.
    ///
    /// # Errors
    /// The stored root is gone or no longer a directory.
    pub fn from_record(record: &WorkspaceRecord) -> Result<Self, WorkspaceError> {
        let path = Path::new(&record.root);
        let root = paths::canonical_dir(path).map_err(|source| WorkspaceError::Root {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self::at(root, record.name.clone(), Some(record.id.clone())))
    }

    fn at(root: PathBuf, name: String, id: Option<WorkspaceId>) -> Self {
        let rules = IgnoreRules::load(&root);
        Self {
            id,
            root,
            name,
            rules: RwLock::new(rules),
            index: Mutex::new(None),
        }
    }

    /// The registry id; `None` for a root opened ad hoc.
    pub fn id(&self) -> Option<&WorkspaceId> {
        self.id.as_ref()
    }

    /// Canonical absolute root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The root as stored in the registry and shown to users.
    pub fn root_string(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    // ------------------------------------------------------------ .harness

    pub fn harness_dir(&self) -> PathBuf {
        self.root.join(HARNESS_DIR)
    }

    pub fn instructions_file(&self) -> PathBuf {
        self.harness_dir().join(INSTRUCTIONS_FILE)
    }

    pub fn has_instructions(&self) -> bool {
        self.instructions_file().is_file()
    }

    /// The project instructions, when present.
    ///
    /// # Errors
    /// The file exists but cannot be read.
    pub fn instructions(&self) -> std::io::Result<Option<String>> {
        match std::fs::read_to_string(self.instructions_file()) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.harness_dir().join(CONFIG_FILE)
    }

    pub fn ignore_file(&self) -> PathBuf {
        self.harness_dir().join(IGNORE_FILE)
    }

    pub fn permissions_file(&self) -> PathBuf {
        self.harness_dir().join(PERMISSIONS_FILE)
    }

    // --------------------------------------------------------------- paths

    /// Resolves a path the mentor gave (root-relative, `/` or native
    /// separators, or absolute when inside the root) to an absolute
    /// path under the root. Existing components are canonicalised, so a
    /// symlink pointing out of the root resolves outside and is refused;
    /// the trailing part may not exist yet (files about to be created).
    ///
    /// # Errors
    /// [`PathError::Outside`] for `..` escapes, absolute paths elsewhere
    /// and symlinks that leave the root; [`PathError::Invalid`] for
    /// malformed input.
    pub fn resolve(&self, user_path: &str) -> Result<PathBuf, PathError> {
        paths::resolve(&self.root, user_path)
    }

    /// Whether an absolute path is the root or under it, after
    /// canonicalising what exists of it (case-insensitively on Windows).
    pub fn contains(&self, abs: &Path) -> bool {
        paths::contains(&self.root, abs)
    }

    /// The mentor-facing form: root-relative with `/` separators (`.`
    /// for the root); a path outside comes back absolute, `/` as well.
    pub fn display(&self, abs: &Path) -> String {
        paths::display(&self.root, abs)
    }

    // -------------------------------------------------------- discovery

    /// The built-in defaults plus `.harness/ignore` as last loaded.
    pub fn ignore_rules(&self) -> IgnoreRules {
        self.rules
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Re-reads `.harness/ignore`.
    pub fn reload_ignore_rules(&self) {
        let rules = IgnoreRules::load(&self.root);
        *self
            .rules
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = rules;
    }

    /// A walk of the root honouring every ignore rule, sorted, symlinks
    /// not followed. The root itself is the first entry.
    pub fn walker(&self) -> ignore::Walk {
        self.ignore_rules().walk_builder().build()
    }

    /// A walk of `dir` (absolute, under the root) with the same rules,
    /// at most `max_depth` levels below it. `dir` itself is the first
    /// entry.
    pub fn walker_at(&self, dir: &Path, max_depth: usize) -> ignore::Walk {
        self.ignore_rules().walk_builder_at(dir, max_depth).build()
    }

    /// Whether `path` (absolute under the root, or root-relative) is
    /// hidden from discovery by the defaults or `.harness/ignore`. Does
    /// not consult `.gitignore` (the walker does).
    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        self.rules
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_ignored(path, is_dir)
    }

    /// The file index, built on first use and rebuilt when older than
    /// [`INDEX_TTL`]. Blocks while building: call from a blocking
    /// context (`spawn_blocking`) on large trees.
    pub fn index(&self) -> Arc<FileIndex> {
        let mut slot = self
            .index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = slot.as_ref().filter(|i| !i.is_stale()) {
            return Arc::clone(index);
        }
        let index = Arc::new(self.build_index());
        *slot = Some(Arc::clone(&index));
        index
    }

    /// The current index without building or refreshing one.
    pub fn cached_index(&self) -> Option<Arc<FileIndex>> {
        self.index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Drops the cached index so the next use rebuilds it (a tool
    /// created or deleted a file).
    pub fn invalidate_index(&self) {
        *self
            .index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Re-reads `.harness/ignore` and rebuilds the index now.
    pub fn refresh(&self) -> Arc<FileIndex> {
        self.reload_ignore_rules();
        let index = Arc::new(self.build_index());
        *self
            .index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&index));
        index
    }

    fn build_index(&self) -> FileIndex {
        let rules = self.ignore_rules();
        let index = FileIndex::build(&self.root, &rules);
        tracing::debug!(
            root = %self.root.display(),
            files = index.len(),
            truncated = index.is_truncated(),
            took = ?index.build_time(),
            "workspace indexed"
        );
        index
    }

    /// `HEAD` of the repository at the root, if it is one.
    pub fn git_head(&self) -> Option<GitHead> {
        git_head(&self.root)
    }
}
