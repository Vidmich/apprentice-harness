//! Writing a bundle: the selected sessions' rows and blobs, through the
//! redaction pass, into a directory or a `.tar.zst`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use apprentice_api::types::{
    BUNDLE_FORMAT_VERSION, BundleManifest, BundleSelection, BundleSession,
};
use serde_json::Value;

use super::format::{Rows, write_blob, write_manifest};
use super::redact::{RedactConfig, Redactor};
use super::{BundleError, is_packed, pack, temp_dir_beside};
use crate::trace::{BLOB_REF_KEY, BlobId, TraceError, TraceStore, kinds, now_ts, sha256_hex};

/// What the redaction pass runs with.
#[derive(Debug, Clone, Default)]
pub struct RedactOptions {
    /// The built-in secret detectors.
    pub builtins: bool,
    /// `<config_dir>/redact.toml`, when there is one.
    pub user: Option<RedactConfig>,
    /// Also read each exported workspace's `.harness/redact.toml`.
    pub workspace_patterns: bool,
    /// Replace the workspace roots and `home` with `<WS>` / `<HOME>`.
    pub paths: bool,
    pub home: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// Where to write: a `.tar.zst` file, or a directory (created; must
    /// not exist or be empty).
    pub output: PathBuf,
    /// Bounds already resolved to store timestamps.
    pub selection: BundleSelection,
    /// `None` = no redaction at all.
    pub redact: Option<RedactOptions>,
}

/// Writes the bundle and returns where it is and its manifest.
///
/// # Errors
/// Nothing selected, a bad output path, a store or I/O failure, or a
/// user pattern that does not compile.
pub fn export(
    store: &TraceStore,
    opts: &ExportOptions,
) -> Result<(PathBuf, BundleManifest), BundleError> {
    let sessions = store.select_bundle_sessions(&opts.selection)?;
    if sessions.is_empty() {
        return Err(BundleError::Invalid(
            "no session matches the selection".to_owned(),
        ));
    }
    let mut rows = store.bundle_rows(&sessions)?;
    let packed = is_packed(&opts.output);
    let work = if packed {
        if let Some(parent) = opts.output.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| BundleError::io("creating", parent, e))?;
        }
        temp_dir_beside(&opts.output)?
    } else {
        prepare_dir(&opts.output)?;
        opts.output.clone()
    };
    let result = write_into(store, &mut rows, opts, &work);
    if packed {
        let packed_result = result.and_then(|m| pack(&work, &opts.output).map(|()| m));
        let _ = std::fs::remove_dir_all(&work);
        if packed_result.is_err() {
            let _ = std::fs::remove_file(&opts.output);
        }
        return packed_result.map(|m| (opts.output.clone(), m));
    }
    match result {
        Ok(m) => Ok((opts.output.clone(), m)),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&work);
            Err(e)
        }
    }
}

/// `dir` must not exist, or be an empty directory.
fn prepare_dir(dir: &Path) -> Result<(), BundleError> {
    match std::fs::read_dir(dir) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(BundleError::Invalid(format!(
                    "{} exists and is not empty",
                    dir.display()
                )));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir).map_err(|e| BundleError::io("creating", dir, e))
        }
        Err(e) if dir.is_file() => Err(BundleError::io("creating", dir, e)),
        Err(e) => Err(BundleError::io("listing", dir, e)),
    }
}

fn write_into(
    store: &TraceStore,
    rows: &mut Rows,
    opts: &ExportOptions,
    dir: &Path,
) -> Result<BundleManifest, BundleError> {
    let mut redactor = match &opts.redact {
        Some(r) => Some(build_redactor(rows, r)?),
        None => None,
    };

    // Blobs first: their new ids go into the rows.
    let mut blob_map: BTreeMap<String, String> = BTreeMap::new();
    let mut new_size: HashMap<String, u64> = HashMap::new();
    for b in &mut rows.blobs {
        if b.pruned {
            continue;
        }
        let bytes = match store.read_blob(&BlobId::from(b.id.as_str())) {
            Ok(bytes) => bytes,
            Err(TraceError::NotFound { .. }) => {
                b.pruned = true;
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let redacted = redactor
            .as_mut()
            .and_then(|r| r.redact_blob(&bytes, &b.media_type));
        match redacted {
            Some(new) => {
                let id = sha256_hex(&new);
                write_blob(dir, &id, &new)?;
                blob_map.insert(b.id.clone(), id.clone());
                new_size.insert(id.clone(), new.len() as u64);
                b.id = id;
                b.size = new.len() as u64;
            }
            None => write_blob(dir, &b.id, &bytes)?,
        }
    }
    rows.blobs.sort_by(|a, b| a.id.cmp(&b.id));
    rows.blobs.dedup_by(|a, b| a.id == b.id);

    let mut touched_requests = 0;
    let mut request_sizes: HashMap<String, u64> = HashMap::new();
    for e in &mut rows.events {
        if let Some(r) = redactor.as_mut() {
            r.redact_value(&mut e.payload);
        }
        if let Some(old) = &e.blob_id
            && let Some(new) = blob_map.get(old)
        {
            e.blob_id = Some(new.clone());
            if e.kind == kinds::MENTOR_REQUEST {
                touched_requests += 1;
                let size = new_size.get(new).copied().unwrap_or(0);
                e.payload["request_hash"] = Value::String(new.clone());
                e.payload["bytes"] = Value::from(size);
                e.payload["redacted"] = Value::Bool(true);
                request_sizes.insert(e.id.clone(), size);
            }
        }
        remap_blob_refs(&mut e.payload, &blob_map);
    }
    for c in &mut rows.mentor_calls {
        if let Some(size) = request_sizes.get(&c.request_event_id) {
            c.request_bytes = Some(i64::try_from(*size).unwrap_or(i64::MAX));
        }
    }
    if let Some(r) = redactor.as_mut() {
        for w in &mut rows.workspaces {
            r.redact_field(&mut w.root);
            r.redact_field(&mut w.name);
            r.redact_value(&mut w.settings);
        }
        for s in &mut rows.sessions {
            r.redact_opt(&mut s.title);
            r.redact_opt(&mut s.workspace_path);
            r.redact_value(&mut s.config);
        }
        for a in &mut rows.agents {
            r.redact_opt(&mut a.task_text);
            r.redact_value(&mut a.options);
        }
        for m in &mut rows.messages {
            r.redact_value(&mut m.content);
        }
    }

    rows.write(dir)?;
    let manifest = BundleManifest {
        format_version: BUNDLE_FORMAT_VERSION,
        created_at: now_ts(),
        harness_version: env!("CARGO_PKG_VERSION").to_owned(),
        schema_version: store.schema_version()?,
        selection: opts.selection.clone(),
        sessions: manifest_sessions(rows),
        counts: rows.counts(),
        redaction: redactor
            .as_ref()
            .map(|r| r.report(blob_map, touched_requests)),
    };
    write_manifest(dir, &manifest)?;
    Ok(manifest)
}

/// The pass for these rows: built-ins, the user's patterns, each
/// workspace's own, and the paths when asked.
fn build_redactor(rows: &Rows, opts: &RedactOptions) -> Result<Redactor, BundleError> {
    let mut roots: Vec<String> = rows.workspaces.iter().map(|w| w.root.clone()).collect();
    for s in &rows.sessions {
        if let Some(p) = &s.workspace_path
            && !roots.contains(p)
        {
            roots.push(p.clone());
        }
    }
    let mut workspace = Vec::new();
    if opts.workspace_patterns {
        for root in &roots {
            let dir = Path::new(root).join(crate::workspace::HARNESS_DIR);
            if let Some(cfg) = RedactConfig::load(&dir)? {
                workspace.push((root.clone(), cfg));
            }
        }
    }
    let redactor = Redactor::new(opts.builtins, opts.user.as_ref(), &workspace)?;
    Ok(if opts.paths {
        redactor.with_paths(&roots, opts.home.as_deref())
    } else {
        redactor
    })
}

/// Rewrites `{"$blob": old}` references to their redacted ids.
fn remap_blob_refs(value: &mut Value, map: &BTreeMap<String, String>) {
    if map.is_empty() {
        return;
    }
    match value {
        Value::Object(fields) => {
            if let Some(Value::String(id)) = fields.get_mut(BLOB_REF_KEY)
                && let Some(new) = map.get(id.as_str())
            {
                *id = new.clone();
            }
            for v in fields.values_mut() {
                remap_blob_refs(v, map);
            }
        }
        Value::Array(items) => {
            for v in items {
                remap_blob_refs(v, map);
            }
        }
        _ => {}
    }
}

fn manifest_sessions(rows: &Rows) -> Vec<BundleSession> {
    rows.sessions
        .iter()
        .map(|s| BundleSession {
            id: s.id.clone(),
            title: s.title.clone(),
            workspace_id: s.workspace_id.clone(),
            created_at: s.created_at.clone(),
            updated_at: s.updated_at.clone(),
            status: s.status.clone(),
            agents: rows.agents.iter().filter(|a| a.session_id == s.id).count() as u64,
            events: rows.events.iter().filter(|e| e.session_id == s.id).count() as u64,
            mentor_calls: rows
                .mentor_calls
                .iter()
                .filter(|c| c.session_id == s.id)
                .count() as u64,
            messages: rows
                .messages
                .iter()
                .filter(|m| m.session_id == s.id)
                .count() as u64,
        })
        .collect()
}
