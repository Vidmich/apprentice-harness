//! Reading a bundle into a store: the manifest is checked, every blob
//! is hashed before anything is written, then the rows go in.

use std::path::PathBuf;

use apprentice_api::types::{BUNDLE_FORMAT_VERSION, BundleCounts, ImportedSession};
use serde_json::json;

use super::format::{Rows, read_blob, read_manifest};
use super::{BundleDir, BundleError};
use crate::trace::{ImportWrite, SCHEMA_VERSION, TraceStore, now_ts, sha256_hex};

#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// A bundle directory or `.tar.zst`.
    pub path: PathBuf,
    /// Attach every session to this registered workspace.
    pub into_workspace: Option<String>,
    /// Keep the bundle's ids (an existing one is a conflict).
    pub keep_ids: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub sessions: Vec<ImportedSession>,
    pub counts: BundleCounts,
    /// The bundle's manifest says a redaction pass changed it.
    pub redacted: bool,
    pub blobs_written: u64,
}

/// Imports the bundle at `opts.path`.
///
/// # Errors
/// Not a bundle, a newer format or schema, a blob that is missing or
/// does not hash to its id (named), an id conflict under `keep_ids`, an
/// unknown workspace.
pub fn import(store: &TraceStore, opts: &ImportOptions) -> Result<ImportReport, BundleError> {
    let dir = BundleDir::open(&opts.path)?;
    let manifest = read_manifest(dir.path())?;
    if manifest.format_version > BUNDLE_FORMAT_VERSION {
        return Err(BundleError::Invalid(format!(
            "bundle format v{} is newer than this build reads (v{BUNDLE_FORMAT_VERSION})",
            manifest.format_version
        )));
    }
    if manifest.schema_version > SCHEMA_VERSION {
        return Err(BundleError::Invalid(format!(
            "bundle rows come from trace schema v{} (this build has v{SCHEMA_VERSION}); \
             upgrade apprentice-harness",
            manifest.schema_version
        )));
    }
    let rows = Rows::read(dir.path())?;
    for b in rows.blobs.iter().filter(|b| !b.pruned) {
        let bytes = read_blob(dir.path(), &b.id)?;
        let actual = sha256_hex(&bytes);
        if actual != b.id {
            return Err(BundleError::BlobCorrupted {
                id: b.id.clone(),
                actual,
            });
        }
    }
    let redacted = manifest.redaction.as_ref().is_some_and(|r| r.applied);
    let write = ImportWrite {
        into_workspace: opts.into_workspace.clone(),
        keep_ids: opts.keep_ids,
        provenance: json!({
            "bundle": opts
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned()),
            "bundle_created_at": manifest.created_at,
            "harness_version": manifest.harness_version,
            "imported_at": now_ts(),
            "redacted": redacted,
        }),
    };
    let outcome = store.import_bundle_rows(dir.path(), &rows, &write)?;
    Ok(ImportReport {
        sessions: outcome.sessions,
        counts: rows.counts(),
        redacted,
        blobs_written: outcome.blobs_written,
    })
}
