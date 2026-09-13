//! Trace bundles (task M01-14): a selection of sessions with everything
//! the store has on them — rows of every table, the blobs they reference
//! — as a directory or a `.tar.zst`, so traces can leave the machine
//! and come back intact.
//!
//! [`export`] writes one (through [`redact::Redactor`] unless told not
//! to), [`import`] reads one into another store after verifying every
//! blob, and [`replay`] proves the stored `mentor.request` bodies are
//! what a replay needs. [`BundleService`] exposes the three over RPC.
//!
//! Layout (`format`):
//!
//! ```text
//! bundle/
//!   manifest.json          format, versions, selection, counts, redaction report
//!   workspaces.jsonl       the workspaces the sessions are on
//!   sessions.jsonl agents.jsonl steps.jsonl events.jsonl
//!   mentor_calls.jsonl session_messages.jsonl blobs.jsonl
//!   blobs/<aa>/<sha256>    the referenced blobs (re-hashed after redaction)
//! ```

mod export;
pub mod format;
mod import;
pub mod redact;
mod replay;
mod rpc;

use std::path::{Path, PathBuf};

use apprentice_api::jsonrpc::RpcError;

pub use export::{ExportOptions, RedactOptions, export};
pub use import::{ImportOptions, ImportReport, import};
pub use redact::{RedactConfig, Redactor};
pub use replay::{ReplaySelection, replay_check};
pub use rpc::BundleService;

use crate::trace::TraceError;

/// Compression level of a `.tar.zst` bundle.
pub const ZSTD_LEVEL: i32 = 6;

/// The packed bundle's suffix.
pub const PACKED_SUFFIX: &str = ".tar.zst";

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error(transparent)]
    Trace(#[from] TraceError),

    #[error("{context} {path}: {source}")]
    Io {
        context: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// A bundle this build cannot read, or bad parameters.
    #[error("{0}")]
    Invalid(String),

    /// A blob file whose content does not hash to its id.
    #[error("blob `{id}` is corrupted (content hashes to {actual})")]
    BlobCorrupted { id: String, actual: String },

    /// A blob row without its file.
    #[error("blob `{id}` is missing from the bundle")]
    BlobMissing { id: String },

    /// An id the store already has (`--keep-ids`).
    #[error("{0}")]
    Conflict(String),

    #[error("{what} `{id}` not found")]
    NotFound { what: &'static str, id: String },

    /// A user pattern that is not a regex.
    #[error("redaction pattern `{name}` in {file}: {reason}")]
    BadPattern {
        name: String,
        file: String,
        reason: String,
    },
}

impl From<rusqlite::Error> for BundleError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Trace(TraceError::from(e))
    }
}

impl BundleError {
    fn io(context: &'static str, path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            context,
            path: path.into(),
            source,
        }
    }

    /// Stable machine-readable kind.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Trace(e) => e.kind(),
            Self::Io { .. } => "io",
            Self::Json(_) => "json",
            Self::Invalid(_) => "invalid",
            Self::BlobCorrupted { .. } => "blob_corrupted",
            Self::BlobMissing { .. } => "blob_missing",
            Self::Conflict(_) => "conflict",
            Self::NotFound { .. } => "not_found",
            Self::BadPattern { .. } => "bad_pattern",
        }
    }
}

impl From<BundleError> for RpcError {
    fn from(e: BundleError) -> Self {
        match e {
            BundleError::Trace(t) => t.into(),
            BundleError::Invalid(_) | BundleError::BadPattern { .. } => {
                RpcError::invalid_params(e.to_string())
            }
            BundleError::Conflict(_) => RpcError::conflict(e.to_string()),
            BundleError::NotFound { .. } => RpcError::not_found(e.to_string()),
            _ => RpcError::internal(format!("trace bundle: {e}"))
                .with_details(serde_json::json!({ "reason": e.kind() })),
        }
    }
}

/// Where a bundle lives: an unpacked directory, or a `.tar.zst` file
/// that is unpacked next to itself while it is read.
#[derive(Debug)]
pub struct BundleDir {
    dir: PathBuf,
    /// A temporary unpack to remove when done.
    temp: bool,
}

impl BundleDir {
    /// `path` as a directory: itself when it is one, else its contents
    /// unpacked into a temporary directory beside it.
    pub fn open(path: &Path) -> Result<Self, BundleError> {
        if path.is_dir() {
            return Ok(Self {
                dir: path.to_path_buf(),
                temp: false,
            });
        }
        if !path.is_file() {
            return Err(BundleError::NotFound {
                what: "bundle",
                id: path.display().to_string(),
            });
        }
        let dir = temp_dir_beside(path)?;
        if let Err(e) = unpack(path, &dir) {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
        Ok(Self { dir, temp: true })
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for BundleDir {
    fn drop(&mut self) {
        if self.temp {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// `<parent>/.<name>.<uuid>.tmp`, created.
fn temp_dir_beside(path: &Path) -> Result<PathBuf, BundleError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let name = path
        .file_name()
        .map_or_else(|| "bundle".to_owned(), |n| n.to_string_lossy().into_owned());
    let dir = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::now_v7().simple()));
    std::fs::create_dir_all(&dir).map_err(|e| BundleError::io("creating temp dir", &dir, e))?;
    Ok(dir)
}

/// Packs `dir` into `out` as `.tar.zst` (entries relative to the
/// bundle root, in sorted order so the same content packs the same).
fn pack(dir: &Path, out: &Path) -> Result<(), BundleError> {
    let file = std::fs::File::create(out).map_err(|e| BundleError::io("creating", out, e))?;
    let encoder =
        zstd::Encoder::new(file, ZSTD_LEVEL).map_err(|e| BundleError::io("compressing", out, e))?;
    let mut tar = tar::Builder::new(encoder);
    tar.mode(tar::HeaderMode::Deterministic);
    let mut files = Vec::new();
    walk(dir, &mut files)?;
    files.sort();
    for path in files {
        let rel = path.strip_prefix(dir).expect("under the bundle dir");
        let name = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let mut f = std::fs::File::open(&path).map_err(|e| BundleError::io("reading", &path, e))?;
        tar.append_file(&name, &mut f)
            .map_err(|e| BundleError::io("packing", &path, e))?;
    }
    let encoder = tar
        .into_inner()
        .map_err(|e| BundleError::io("packing", out, e))?;
    encoder
        .finish()
        .map_err(|e| BundleError::io("compressing", out, e))?;
    Ok(())
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), BundleError> {
    for entry in std::fs::read_dir(dir).map_err(|e| BundleError::io("listing", dir, e))? {
        let entry = entry.map_err(|e| BundleError::io("listing", dir, e))?;
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

/// Unpacks a `.tar.zst` into `dir`; entries must stay inside it.
fn unpack(archive: &Path, dir: &Path) -> Result<(), BundleError> {
    let file = std::fs::File::open(archive).map_err(|e| BundleError::io("opening", archive, e))?;
    let decoder =
        zstd::Decoder::new(file).map_err(|e| BundleError::io("decompressing", archive, e))?;
    let mut tar = tar::Archive::new(decoder);
    for entry in tar
        .entries()
        .map_err(|e| BundleError::io("unpacking", archive, e))?
    {
        let mut entry = entry.map_err(|e| BundleError::io("unpacking", archive, e))?;
        let rel = entry
            .path()
            .map_err(|e| BundleError::io("unpacking", archive, e))?
            .into_owned();
        if rel
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(BundleError::Invalid(format!(
                "{}: entry `{}` leaves the bundle",
                archive.display(),
                rel.display()
            )));
        }
        let target = dir.join(&rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| BundleError::io("creating", parent, e))?;
        }
        entry
            .unpack(&target)
            .map_err(|e| BundleError::io("unpacking", &target, e))?;
    }
    Ok(())
}

/// Whether `path` names a packed bundle.
pub fn is_packed(path: &Path) -> bool {
    path.to_string_lossy()
        .to_ascii_lowercase()
        .ends_with(PACKED_SUFFIX)
}
