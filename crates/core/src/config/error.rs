//! Configuration errors. Every variant names the file and key involved where
//! one exists, so CLI and GUI can show actionable messages.

use std::path::PathBuf;

use apprentice_api::jsonrpc::RpcError;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not valid TOML: {message}")]
    Syntax { path: PathBuf, message: String },

    #[error("{path}: unknown key `{key}`")]
    UnknownKey { path: PathBuf, key: String },

    #[error("{path}: key `{key}` cannot be set in a workspace config (allowed: {allowed})")]
    KeyNotOverridable {
        path: PathBuf,
        key: String,
        allowed: String,
    },

    #[error("{path}: key `{key}` expects {expected}, found {found}")]
    WrongType {
        path: PathBuf,
        key: String,
        expected: &'static str,
        found: &'static str,
    },

    #[error("{path}: {message}")]
    Invalid { path: PathBuf, message: String },

    #[error("key `{key}` is a table, not a value; set a leaf key such as `{key}.<field>`")]
    NotALeaf { key: String },

    #[error("key `{key}` is not a valid dotted key: {reason}")]
    BadKey { key: String, reason: &'static str },

    #[error("no such key `{key}`")]
    NoSuchKey { key: String },

    #[error("a workspace path is required to edit the workspace layer")]
    WorkspaceRequired,

    #[error("environment variable {var} is not valid UTF-8")]
    BadEnv { var: String },

    #[error(transparent)]
    NoHomeDir(#[from] super::paths::NoHomeDir),
}

impl ConfigError {
    /// Stable machine-readable kind for RPC `data.kind` details.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Read { .. } => "read",
            Self::Write { .. } => "write",
            Self::Syntax { .. } => "syntax",
            Self::UnknownKey { .. } => "unknown_key",
            Self::KeyNotOverridable { .. } => "key_not_overridable",
            Self::WrongType { .. } => "wrong_type",
            Self::Invalid { .. } => "invalid",
            Self::NotALeaf { .. } => "not_a_leaf",
            Self::BadKey { .. } => "bad_key",
            Self::NoSuchKey { .. } => "no_such_key",
            Self::WorkspaceRequired => "workspace_required",
            Self::BadEnv { .. } => "bad_env",
            Self::NoHomeDir(_) => "no_home_dir",
        }
    }

    fn key(&self) -> Option<&str> {
        match self {
            Self::UnknownKey { key, .. }
            | Self::KeyNotOverridable { key, .. }
            | Self::WrongType { key, .. }
            | Self::NotALeaf { key }
            | Self::BadKey { key, .. }
            | Self::NoSuchKey { key } => Some(key),
            _ => None,
        }
    }

    fn path(&self) -> Option<&PathBuf> {
        match self {
            Self::Read { path, .. }
            | Self::Write { path, .. }
            | Self::Syntax { path, .. }
            | Self::UnknownKey { path, .. }
            | Self::KeyNotOverridable { path, .. }
            | Self::WrongType { path, .. }
            | Self::Invalid { path, .. } => Some(path),
            _ => None,
        }
    }
}

impl From<ConfigError> for RpcError {
    fn from(e: ConfigError) -> Self {
        let code = match &e {
            ConfigError::NoSuchKey { .. } => return RpcError::not_found(e.to_string()),
            _ => RpcError::config(e.to_string()),
        };
        let mut details = serde_json::Map::new();
        details.insert("reason".into(), e.kind().into());
        if let Some(k) = e.key() {
            details.insert("key".into(), k.into());
        }
        if let Some(p) = e.path() {
            details.insert("file".into(), p.display().to_string().into());
        }
        code.with_details(details.into())
    }
}
