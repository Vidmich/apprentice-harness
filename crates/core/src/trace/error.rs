//! Trace store errors and their RPC mapping.

use std::path::PathBuf;

use apprentice_api::jsonrpc::RpcError;

#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("{context} {path}: {source}")]
    Io {
        context: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The database was written by a newer harness; never downgraded.
    #[error(
        "trace database schema v{found} is newer than this build supports (v{supported}); \
         upgrade apprentice-harness or point HARNESS_HOME at another directory"
    )]
    SchemaTooNew { found: u32, supported: u32 },

    #[error("migration to schema v{version} failed: {source}")]
    Migration {
        version: u32,
        #[source]
        source: rusqlite::Error,
    },

    #[error("{what} `{id}` not found")]
    NotFound { what: &'static str, id: String },

    /// A blob's file content does not hash to its id.
    #[error("blob `{id}` is corrupted (content hashes to {actual})")]
    BlobCorrupted { id: String, actual: String },

    #[error("invalid {what}: {reason}")]
    Invalid { what: &'static str, reason: String },

    /// The row exists already (an import under a taken id).
    #[error("{0}")]
    Conflict(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// The writer thread is gone (daemon shutting down).
    #[error("trace writer closed")]
    WriterClosed,

    /// The transaction holding this event (and others) failed to commit.
    #[error("batch commit failed: {0}")]
    BatchFailed(String),
}

impl TraceError {
    pub(super) fn io(
        context: &'static str,
        path: impl Into<PathBuf>,
        source: std::io::Error,
    ) -> Self {
        Self::Io {
            context,
            path: path.into(),
            source,
        }
    }

    pub(super) fn not_found(what: &'static str, id: impl Into<String>) -> Self {
        Self::NotFound {
            what,
            id: id.into(),
        }
    }

    pub(super) fn invalid(what: &'static str, reason: impl Into<String>) -> Self {
        Self::Invalid {
            what,
            reason: reason.into(),
        }
    }

    /// Stable machine-readable kind.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Sqlite(_) => "sqlite",
            Self::Io { .. } => "io",
            Self::SchemaTooNew { .. } => "schema_too_new",
            Self::Migration { .. } => "migration",
            Self::NotFound { .. } => "not_found",
            Self::BlobCorrupted { .. } => "blob_corrupted",
            Self::Invalid { .. } => "invalid",
            Self::Conflict(_) => "conflict",
            Self::Json(_) => "json",
            Self::WriterClosed => "writer_closed",
            Self::BatchFailed(_) => "batch_failed",
        }
    }
}

impl From<TraceError> for RpcError {
    fn from(e: TraceError) -> Self {
        match &e {
            TraceError::NotFound { .. } => RpcError::not_found(e.to_string()),
            TraceError::Invalid { .. } => RpcError::invalid_params(e.to_string()),
            TraceError::Conflict(_) => RpcError::conflict(e.to_string()),
            _ => RpcError::internal(format!("trace store: {e}"))
                .with_details(serde_json::json!({ "reason": e.kind() })),
        }
    }
}
