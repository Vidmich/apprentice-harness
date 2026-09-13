//! The rows of a bundle, one struct per `.jsonl` file, and the files
//! themselves. A row is the table's columns as a JSON object; the JSON
//! columns (`config_json`, `payload_json`, ...) are objects, not
//! strings, so any reader gets them parsed.

use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};

use apprentice_api::types::{BundleCounts, BundleManifest};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use super::BundleError;

/// `manifest.json`.
pub const MANIFEST_FILE: &str = "manifest.json";
/// The `.jsonl` files, in the order they are written.
pub const ROW_FILES: &[&str] = &[
    "workspaces.jsonl",
    "sessions.jsonl",
    "agents.jsonl",
    "steps.jsonl",
    "events.jsonl",
    "mentor_calls.jsonl",
    "session_messages.jsonl",
    "blobs.jsonl",
];
/// The blob directory (`blobs/<aa>/<sha256>`).
pub const BLOB_DIR: &str = "blobs";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRow {
    pub id: String,
    pub root: String,
    pub name: String,
    pub created_at: String,
    pub last_used_at: String,
    #[serde(default)]
    pub settings: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRow {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub title_source: Option<String>,
    #[serde(default)]
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub config: Value,
    pub status: String,
    #[serde(default)]
    pub message_count: u64,
    #[serde(default)]
    pub last_agent_status: Option<String>,
    #[serde(default)]
    pub prompt_version: Option<String>,
    #[serde(default)]
    pub tools_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRow {
    pub id: String,
    pub session_id: String,
    #[serde(default)]
    pub parent_agent_id: Option<String>,
    pub kind: String,
    pub created_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    pub status: String,
    #[serde(default)]
    pub task_text: Option<String>,
    #[serde(default)]
    pub options: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRow {
    pub id: String,
    pub agent_id: String,
    pub seq: u64,
    pub started_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRow {
    pub id: String,
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub step_id: Option<String>,
    pub seq: u64,
    pub ts: String,
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub blob_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRow {
    pub id: String,
    pub session_id: String,
    pub agent_id: String,
    pub step_id: String,
    pub request_event_id: String,
    #[serde(default)]
    pub response_event_id: Option<String>,
    pub model: String,
    #[serde(default)]
    pub effort: Option<String>,
    pub started_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    pub status: String,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub http_status: Option<i64>,
    #[serde(default)]
    pub input_tokens: Option<i64>,
    #[serde(default)]
    pub output_tokens: Option<i64>,
    #[serde(default)]
    pub cache_read_tokens: Option<i64>,
    #[serde(default)]
    pub cache_creation_tokens: Option<i64>,
    #[serde(default)]
    pub cost_micros: Option<i64>,
    #[serde(default)]
    pub first_byte_ms: Option<i64>,
    #[serde(default)]
    pub total_ms: Option<i64>,
    #[serde(default)]
    pub request_bytes: Option<i64>,
    #[serde(default)]
    pub apprentice_applied: bool,
    #[serde(default = "default_kind")]
    pub kind: String,
}

fn default_kind() -> String {
    "step".to_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageRow {
    pub session_id: String,
    pub seq: u64,
    pub role: String,
    #[serde(default)]
    pub content: Value,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub step_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRow {
    /// SHA-256 hex of the file under `blobs/`.
    pub id: String,
    pub size: u64,
    pub media_type: String,
    pub created_at: String,
    /// The store had no file for it (retention); the row is kept so
    /// events stay consistent, and the bundle has no file either.
    #[serde(default)]
    pub pruned: bool,
}

/// Every row of a bundle, in file order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rows {
    pub workspaces: Vec<WorkspaceRow>,
    pub sessions: Vec<SessionRow>,
    pub agents: Vec<AgentRow>,
    pub steps: Vec<StepRow>,
    pub events: Vec<EventRow>,
    pub mentor_calls: Vec<CallRow>,
    pub messages: Vec<MessageRow>,
    pub blobs: Vec<BlobRow>,
}

impl Rows {
    /// Row counts (blob bytes from the rows, pruned ones excluded).
    pub fn counts(&self) -> BundleCounts {
        BundleCounts {
            sessions: self.sessions.len() as u64,
            workspaces: self.workspaces.len() as u64,
            agents: self.agents.len() as u64,
            steps: self.steps.len() as u64,
            events: self.events.len() as u64,
            mentor_calls: self.mentor_calls.len() as u64,
            messages: self.messages.len() as u64,
            blobs: self.blobs.len() as u64,
            blob_bytes: self
                .blobs
                .iter()
                .filter(|b| !b.pruned)
                .map(|b| b.size)
                .sum(),
        }
    }

    /// Writes every `.jsonl` file into `dir`.
    pub fn write(&self, dir: &Path) -> Result<(), BundleError> {
        write_jsonl(&dir.join(ROW_FILES[0]), &self.workspaces)?;
        write_jsonl(&dir.join(ROW_FILES[1]), &self.sessions)?;
        write_jsonl(&dir.join(ROW_FILES[2]), &self.agents)?;
        write_jsonl(&dir.join(ROW_FILES[3]), &self.steps)?;
        write_jsonl(&dir.join(ROW_FILES[4]), &self.events)?;
        write_jsonl(&dir.join(ROW_FILES[5]), &self.mentor_calls)?;
        write_jsonl(&dir.join(ROW_FILES[6]), &self.messages)?;
        write_jsonl(&dir.join(ROW_FILES[7]), &self.blobs)?;
        Ok(())
    }

    /// Reads every `.jsonl` file of `dir` (a missing file is empty).
    pub fn read(dir: &Path) -> Result<Self, BundleError> {
        Ok(Self {
            workspaces: read_jsonl(&dir.join(ROW_FILES[0]))?,
            sessions: read_jsonl(&dir.join(ROW_FILES[1]))?,
            agents: read_jsonl(&dir.join(ROW_FILES[2]))?,
            steps: read_jsonl(&dir.join(ROW_FILES[3]))?,
            events: read_jsonl(&dir.join(ROW_FILES[4]))?,
            mentor_calls: read_jsonl(&dir.join(ROW_FILES[5]))?,
            messages: read_jsonl(&dir.join(ROW_FILES[6]))?,
            blobs: read_jsonl(&dir.join(ROW_FILES[7]))?,
        })
    }
}

/// `<dir>/blobs/<aa>/<id>`.
pub fn blob_path(dir: &Path, id: &str) -> PathBuf {
    let shard = id.get(..2).unwrap_or("__");
    dir.join(BLOB_DIR).join(shard).join(id)
}

/// Writes a blob file (a second write of the same id is a no-op).
pub fn write_blob(dir: &Path, id: &str, bytes: &[u8]) -> Result<(), BundleError> {
    let path = blob_path(dir, id);
    if path.is_file() {
        return Ok(());
    }
    let parent = path.parent().expect("blob path has a shard dir");
    fs::create_dir_all(parent).map_err(|e| BundleError::io("creating blob dir", parent, e))?;
    fs::write(&path, bytes).map_err(|e| BundleError::io("writing blob", &path, e))
}

pub fn read_blob(dir: &Path, id: &str) -> Result<Vec<u8>, BundleError> {
    let path = blob_path(dir, id);
    fs::read(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            BundleError::BlobMissing { id: id.to_owned() }
        } else {
            BundleError::io("reading blob", &path, e)
        }
    })
}

pub fn write_manifest(dir: &Path, manifest: &BundleManifest) -> Result<(), BundleError> {
    let path = dir.join(MANIFEST_FILE);
    let mut text = serde_json::to_string_pretty(manifest)?;
    text.push('\n');
    fs::write(&path, text).map_err(|e| BundleError::io("writing manifest", &path, e))
}

pub fn read_manifest(dir: &Path) -> Result<BundleManifest, BundleError> {
    let path = dir.join(MANIFEST_FILE);
    let text = fs::read_to_string(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            BundleError::Invalid(format!(
                "{} is not a bundle: no manifest.json",
                dir.display()
            ))
        } else {
            BundleError::io("reading manifest", &path, e)
        }
    })?;
    serde_json::from_str(&text)
        .map_err(|e| BundleError::Invalid(format!("manifest.json does not parse: {e}")))
}

fn write_jsonl<T: Serialize>(path: &Path, rows: &[T]) -> Result<(), BundleError> {
    let file = fs::File::create(path).map_err(|e| BundleError::io("writing", path, e))?;
    let mut out = std::io::BufWriter::new(file);
    for row in rows {
        serde_json::to_writer(&mut out, row)?;
        out.write_all(b"\n")
            .map_err(|e| BundleError::io("writing", path, e))?;
    }
    out.flush().map_err(|e| BundleError::io("writing", path, e))
}

fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, BundleError> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(BundleError::io("reading", path, e)),
    };
    let mut rows = Vec::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| BundleError::io("reading", path, e))?;
        if line.trim().is_empty() {
            continue;
        }
        let row = serde_json::from_str(&line).map_err(|e| {
            BundleError::Invalid(format!(
                "{} line {}: {e}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                i + 1
            ))
        })?;
        rows.push(row);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_round_trip_through_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let rows = Rows {
            sessions: vec![SessionRow {
                id: "s1".into(),
                created_at: "t0".into(),
                updated_at: "t1".into(),
                title: Some("fix, tests".into()),
                title_source: Some("user".into()),
                workspace_path: None,
                workspace_id: None,
                config: serde_json::json!({"mentor": {"model": "m"}}),
                status: "open".into(),
                message_count: 2,
                last_agent_status: Some("ok".into()),
                prompt_version: Some("v1".into()),
                tools_hash: None,
            }],
            blobs: vec![BlobRow {
                id: "ab".repeat(32),
                size: 3,
                media_type: "text/plain".into(),
                created_at: "t0".into(),
                pruned: false,
            }],
            ..Rows::default()
        };
        rows.write(dir.path()).unwrap();
        let back = Rows::read(dir.path()).unwrap();
        assert_eq!(back, rows);
        assert_eq!(back.counts().sessions, 1);
        assert_eq!(back.counts().blob_bytes, 3);
        write_blob(dir.path(), "abcd", b"xyz").unwrap();
        assert_eq!(read_blob(dir.path(), "abcd").unwrap(), b"xyz");
        assert!(matches!(
            read_blob(dir.path(), "ffff"),
            Err(BundleError::BlobMissing { .. })
        ));
        let missing = Rows::read(&dir.path().join("nope")).unwrap();
        assert!(missing.sessions.is_empty());
    }
}
