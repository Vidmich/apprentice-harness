//! The synchronous store: schema, write API, query API, integrity and size
//! accounting. One `Mutex<Connection>`; every logical action is a single
//! `BEGIN IMMEDIATE` transaction, so per-session `seq` never has gaps.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use apprentice_api::types::{EventSummary, TraceEvent, Usage};
use apprentice_common::paths::Paths;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde_json::Value;

use super::blobs::BlobFiles;
use super::error::TraceError;
use super::payload::{SPILLED_MEDIA_TYPE, blob_refs, spill_strings};
use super::{
    AgentId, AgentKind, BLOB_DIR, BlobId, CallId, CallKind, DB_FILE, EventId, RunStatus, SessionId,
    SessionStatus, StepId, TitleSource, WorkspaceId, kinds, now_ts, schema,
};
use crate::config::TraceConfig;

type Result<T> = std::result::Result<T, TraceError>;

/// Upper bound on `limit` for list queries.
pub const MAX_PAGE: u32 = 10_000;
pub(super) const DEFAULT_PAGE: u32 = 200;

// ------------------------------------------------------------------ inputs

#[derive(Debug, Clone, Default)]
pub struct NewSession {
    pub title: Option<String>,
    /// The workspace root as given at creation (kept as history even
    /// after the workspace is removed from the registry).
    pub workspace_path: Option<String>,
    /// The registry row the session attaches to, when it has one.
    pub workspace_id: Option<WorkspaceId>,
    /// Resolved config snapshot at creation.
    pub config: Value,
}

#[derive(Debug, Clone)]
pub struct NewAgent {
    pub session: SessionId,
    pub parent: Option<AgentId>,
    pub kind: AgentKind,
    pub task_text: Option<String>,
    pub options: Value,
}

impl NewAgent {
    pub fn main(session: SessionId, task_text: impl Into<String>) -> Self {
        Self {
            session,
            parent: None,
            kind: AgentKind::Main,
            task_text: Some(task_text.into()),
            options: Value::Object(serde_json::Map::new()),
        }
    }
}

/// Large payload attached to an event.
#[derive(Debug, Clone)]
pub enum BlobInput {
    Bytes {
        data: Vec<u8>,
        media_type: String,
    },
    /// Reference an already stored blob (its refcount is incremented).
    Existing(BlobId),
}

#[derive(Debug, Clone)]
pub struct NewEvent {
    pub session: SessionId,
    pub agent: Option<AgentId>,
    pub step: Option<StepId>,
    pub kind: String,
    pub payload: Value,
    pub blob: Option<BlobInput>,
}

impl NewEvent {
    pub fn new(session: SessionId, kind: impl Into<String>) -> Self {
        Self {
            session,
            agent: None,
            step: None,
            kind: kind.into(),
            payload: Value::Object(serde_json::Map::new()),
            blob: None,
        }
    }

    #[must_use]
    pub fn agent(mut self, agent: AgentId) -> Self {
        self.agent = Some(agent);
        self
    }

    #[must_use]
    pub fn step(mut self, step: StepId) -> Self {
        self.step = Some(step);
        self
    }

    #[must_use]
    pub fn payload(mut self, payload: Value) -> Self {
        self.payload = payload;
        self
    }

    #[must_use]
    pub fn blob_bytes(mut self, data: impl Into<Vec<u8>>, media_type: impl Into<String>) -> Self {
        self.blob = Some(BlobInput::Bytes {
            data: data.into(),
            media_type: media_type.into(),
        });
        self
    }

    #[must_use]
    pub fn blob(mut self, id: BlobId) -> Self {
        self.blob = Some(BlobInput::Existing(id));
        self
    }
}

/// Insert into `mentor_calls` when a call starts.
#[derive(Debug, Clone)]
pub struct MentorCallStart {
    pub id: CallId,
    pub session: SessionId,
    pub agent: AgentId,
    pub step: StepId,
    pub request_event: EventId,
    pub model: String,
    pub effort: Option<String>,
    pub request_bytes: Option<u64>,
    /// Store timestamp; `None` = now. Set by imports and tests.
    pub started_at: Option<String>,
    /// A loop step, or the session title (task M01-10).
    pub kind: CallKind,
}

/// Update of `mentor_calls` when a call ends.
#[derive(Debug, Clone)]
pub struct MentorCallEnd {
    pub id: CallId,
    pub response_event: Option<EventId>,
    pub status: RunStatus,
    pub stop_reason: Option<String>,
    pub http_status: Option<u16>,
    pub usage: Option<Usage>,
    /// USD × 1e6; `None` when the model has no pricing (token accounting).
    pub cost_micros: Option<i64>,
    pub first_byte_ms: Option<u64>,
    pub total_ms: Option<u64>,
}

impl MentorCallEnd {
    pub fn new(id: CallId, status: RunStatus) -> Self {
        Self {
            id,
            response_event: None,
            status,
            stop_reason: None,
            http_status: None,
            usage: None,
            cost_micros: None,
            first_byte_ms: None,
            total_ms: None,
        }
    }
}

// ----------------------------------------------------------------- records

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub id: SessionId,
    pub created_at: String,
    pub updated_at: String,
    pub title: Option<String>,
    /// `None` until a title is set (task M01-10).
    pub title_source: Option<TitleSource>,
    pub workspace_path: Option<String>,
    pub workspace_id: Option<WorkspaceId>,
    pub config: Value,
    pub status: SessionStatus,
    /// Rows in `session_messages`.
    pub message_count: u64,
    /// How the last agent on the session ended.
    pub last_agent_status: Option<RunStatus>,
    /// The prompt version and tool set the session started under, set
    /// at its first run.
    pub prompt_version: Option<String>,
    pub tools_hash: Option<String>,
}

/// A registered workspace (schema v2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRecord {
    pub id: WorkspaceId,
    /// Canonical absolute root path, as stored.
    pub root: String,
    pub name: String,
    pub created_at: String,
    pub last_used_at: String,
    pub settings: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentRecord {
    pub id: AgentId,
    pub session_id: SessionId,
    pub parent_agent_id: Option<AgentId>,
    pub kind: AgentKind,
    pub created_at: String,
    pub ended_at: Option<String>,
    pub status: RunStatus,
    pub task_text: Option<String>,
    pub options: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRecord {
    pub id: StepId,
    pub agent_id: AgentId,
    pub seq: u64,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub status: RunStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobMeta {
    pub id: BlobId,
    pub size: u64,
    pub media_type: String,
    pub created_at: String,
    pub refcount: u64,
    pub pruned_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentorCallRow {
    pub id: CallId,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub step_id: StepId,
    pub request_event_id: EventId,
    pub response_event_id: Option<EventId>,
    pub model: String,
    pub effort: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub status: RunStatus,
    pub stop_reason: Option<String>,
    pub http_status: Option<u16>,
    pub usage: Option<Usage>,
    pub cost_micros: Option<i64>,
    pub first_byte_ms: Option<u64>,
    pub total_ms: Option<u64>,
    pub request_bytes: Option<u64>,
    pub apprentice_applied: bool,
    pub kind: CallKind,
}

// ----------------------------------------------------------------- queries

/// Filter for `list_events`. Results are ascending by `seq`; with
/// `before_seq` the newest `limit` rows below it are returned (still
/// ascending), which pages backwards.
#[derive(Debug, Clone, Default)]
pub struct EventQuery {
    pub session_id: Option<SessionId>,
    pub agent_id: Option<AgentId>,
    pub step_id: Option<StepId>,
    pub kinds: Vec<String>,
    pub before_seq: Option<u64>,
    pub after_seq: Option<u64>,
    pub limit: Option<u32>,
}

impl EventQuery {
    pub fn session(session: SessionId) -> Self {
        Self {
            session_id: Some(session),
            ..Self::default()
        }
    }
}

/// Filter for `list_mentor_calls` / `stats`. Times compare against
/// `started_at` (inclusive `since`, exclusive `until`).
#[derive(Debug, Clone, Default)]
pub struct CallFilter {
    pub session_id: Option<SessionId>,
    pub agent_id: Option<AgentId>,
    pub model: Option<String>,
    pub status: Option<RunStatus>,
    pub since: Option<String>,
    pub until: Option<String>,
    /// Only calls of sessions on this workspace (task M01-13).
    pub workspace_id: Option<String>,
}

/// One row of [`TraceStore::list_calls_page`]: the call with what the
/// `stats.calls` table shows of its session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallListRow {
    pub call: MentorCallRow,
    pub session_title: Option<String>,
    pub workspace_id: Option<String>,
}

impl CallFilter {
    pub fn session_of(session: &SessionId) -> Self {
        Self {
            session_id: Some(session.clone()),
            ..Self::default()
        }
    }
}

/// Sums over `mentor_calls` (the raw material of token accounting).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UsageTotals {
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cost_micros: i64,
    /// Completed calls whose model had no pricing entry.
    pub unpriced_calls: u64,
}

/// Grouping for [`TraceStore::stats_by`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    /// Key = model id.
    Model,
    /// Key = `YYYY-MM-DD` of the call start shifted by this many seconds
    /// (the caller's local offset).
    Day { offset_secs: i32 },
    /// Key = session id, label = session title.
    Session,
    /// Key = workspace id (`""` for sessions without one), label = the
    /// workspace root (task M01-13).
    Workspace,
    /// Key = call kind (`step` | `title`).
    Kind,
}

/// One group of [`TraceStore::stats_by`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupedTotals {
    pub key: String,
    pub label: Option<String>,
    pub totals: UsageTotals,
}

/// Outcome of [`TraceStore::reprice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RepriceReport {
    /// Completed calls with usage that matched the filter.
    pub examined: u64,
    /// Rows whose stored cost differed from the recomputed one.
    pub changed: u64,
    /// Matching calls the pricer could not price (cost set to `NULL`).
    pub unpriced: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IntegrityReport {
    /// `PRAGMA integrity_check` output (`"ok"` when healthy).
    pub sqlite: String,
    pub blobs_checked: u64,
    /// Blob rows (not pruned) whose file is gone.
    pub missing_blobs: Vec<BlobId>,
    /// Blob files whose content no longer hashes to their id.
    pub corrupted_blobs: Vec<BlobId>,
    /// Payload references to blobs without a row.
    pub dangling_refs: Vec<(EventId, BlobId)>,
    /// `mentor.request` events without a body blob.
    pub requests_without_blob: Vec<EventId>,
}

impl IntegrityReport {
    pub fn is_ok(&self) -> bool {
        self.sqlite == "ok"
            && self.missing_blobs.is_empty()
            && self.corrupted_blobs.is_empty()
            && self.dangling_refs.is_empty()
            && self.requests_without_blob.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiskUsage {
    pub database_bytes: u64,
    pub wal_bytes: u64,
    pub blob_bytes: u64,
    pub blob_files: u64,
    pub sessions: u64,
    pub events: u64,
    pub blob_rows: u64,
}

impl DiskUsage {
    pub fn total_bytes(&self) -> u64 {
        self.database_bytes + self.wal_bytes + self.blob_bytes
    }
}

// ------------------------------------------------------------------- store

/// Event prepared outside the lock: kind validated, oversized strings and
/// the primary blob already on disk, rows to register collected.
pub(super) struct Prepared {
    pub(super) ev: NewEvent,
    blob_id: Option<BlobId>,
    /// `(id, size, media_type)` for blobs written from bytes.
    register: Vec<(BlobId, usize, String)>,
    /// Existing blob to reference.
    reference: Option<BlobId>,
}

pub struct TraceStore {
    conn: Mutex<Connection>,
    pub(super) files: BlobFiles,
    db_path: PathBuf,
    inline_max: usize,
}

impl std::fmt::Debug for TraceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraceStore")
            .field("db_path", &self.db_path)
            .field("blob_root", &self.files.root())
            .field("inline_max", &self.inline_max)
            .finish_non_exhaustive()
    }
}

impl TraceStore {
    /// Opens `<data_dir>/traces.sqlite` with default trace config, running
    /// pending migrations.
    pub fn open(paths: &Paths) -> Result<Self> {
        Self::open_with(paths, &TraceConfig::default())
    }

    pub fn open_with(paths: &Paths, config: &TraceConfig) -> Result<Self> {
        Self::open_at(
            paths.data_dir.join(DB_FILE),
            paths.data_dir.join(BLOB_DIR),
            usize::try_from(config.inline_payload_max_bytes).unwrap_or(usize::MAX),
        )
    }

    /// Opens explicit paths (tests, tools).
    pub fn open_at(db_path: PathBuf, blob_root: PathBuf, inline_max: usize) -> Result<Self> {
        if let Some(dir) = db_path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| TraceError::io("creating data dir", dir, e))?;
        }
        let mut conn = Connection::open(&db_path)?;
        schema::configure(&conn)?;
        let applied = schema::migrate(&mut conn)?;
        tracing::debug!(db = %db_path.display(), ?applied, "trace store opened");
        Ok(Self {
            conn: Mutex::new(conn),
            files: BlobFiles::new(blob_root),
            db_path,
            inline_max,
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn blob_root(&self) -> &Path {
        self.files.root()
    }

    /// Path of a blob's file (whether or not it exists).
    pub fn blob_path(&self, id: &BlobId) -> PathBuf {
        self.files.path(id.as_str())
    }

    pub fn schema_version(&self) -> Result<u32> {
        schema::version(&self.lock())
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    // ------------------------------------------------------------ sessions

    /// Inserts the session and its `session.created` event.
    pub fn create_session(&self, s: &NewSession) -> Result<SessionId> {
        let id = SessionId::generate();
        let now = now_ts();
        let created = self.prepare(NewEvent::new(id.clone(), kinds::SESSION_CREATED).payload(
            serde_json::json!({
                "title": s.title,
                "workspace_path": s.workspace_path,
                "workspace_id": s.workspace_id,
            }),
        ))?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO sessions(id, created_at, updated_at, title, title_source, workspace_path,
                                  workspace_id, config_json, status)
             VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6, ?7, 'open')",
            params![
                id,
                now,
                s.title,
                s.title.as_ref().map(|_| TitleSource::User.as_str()),
                s.workspace_path,
                s.workspace_id,
                s.config.to_string()
            ],
        )?;
        if let Some(ws) = &s.workspace_id {
            tx.execute(
                "UPDATE workspaces SET last_used_at = ?2 WHERE id = ?1",
                params![ws, now],
            )?;
        }
        insert_event(&tx, created)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn get_session(&self, id: &SessionId) -> Result<SessionRecord> {
        self.lock()
            .query_row(
                &format!("{SESSION_COLUMNS} WHERE id = ?1"),
                params![id],
                session_record,
            )
            .optional()?
            .ok_or_else(|| TraceError::not_found("session", id.as_str()))
    }

    /// Number of sessions with `status`.
    pub fn count_sessions(&self, status: SessionStatus) -> Result<u64> {
        let n: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM sessions WHERE status = ?1",
            params![status.as_str()],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    pub fn set_session_status(&self, id: &SessionId, status: SessionStatus) -> Result<()> {
        let n = self.lock().execute(
            "UPDATE sessions SET status = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, status.as_str(), now_ts()],
        )?;
        require_row(n, "session", id.as_str())
    }

    /// Sets the title and where it came from. A generated title never
    /// replaces the user's: the update is skipped (and `false`
    /// returned) when the stored source is `user` and `source` is not.
    pub fn set_session_title(
        &self,
        id: &SessionId,
        title: Option<&str>,
        source: TitleSource,
    ) -> Result<bool> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE sessions SET title = ?2, title_source = ?3, updated_at = ?4
             WHERE id = ?1 AND (title_source IS NULL OR title_source != 'user' OR ?3 = 'user')",
            params![id, title, source.as_str(), now_ts()],
        )?;
        if n > 0 {
            return Ok(true);
        }
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sessions WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        if exists {
            Ok(false)
        } else {
            Err(TraceError::not_found("session", id.as_str()))
        }
    }

    /// Records the prompt version and tool set a session runs under
    /// (task M01-10); read back on resume.
    pub fn set_session_prefix(
        &self,
        id: &SessionId,
        prompt_version: &str,
        tools_hash: &str,
    ) -> Result<()> {
        let n = self.lock().execute(
            "UPDATE sessions SET prompt_version = ?2, tools_hash = ?3 WHERE id = ?1",
            params![id, prompt_version, tools_hash],
        )?;
        require_row(n, "session", id.as_str())
    }

    // -------------------------------------------------------------- agents

    /// Inserts the agent and its `agent.started` event.
    pub fn start_agent(&self, a: &NewAgent) -> Result<AgentId> {
        let id = AgentId::generate();
        let started = self.prepare(
            NewEvent::new(a.session.clone(), kinds::AGENT_STARTED)
                .agent(id.clone())
                .payload(serde_json::json!({
                    "kind": a.kind,
                    "task_text": a.task_text,
                    "options": a.options,
                    "parent_agent_id": a.parent,
                })),
        )?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO agents(id, session_id, parent_agent_id, kind, created_at, status, task_text, options_json)
             VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7)",
            params![
                id,
                a.session,
                a.parent,
                a.kind.as_str(),
                now_ts(),
                a.task_text,
                a.options.to_string()
            ],
        )?;
        insert_event(&tx, started)?;
        tx.commit()?;
        Ok(id)
    }

    /// Marks the agent ended, appends `agent.finished {status, error?}`
    /// and remembers the status on the session.
    pub fn finish_agent(
        &self,
        id: &AgentId,
        status: RunStatus,
        error: Option<Value>,
    ) -> Result<()> {
        let session = self.get_agent(id)?.session_id;
        let session_for_status = session.clone();
        let mut payload = serde_json::json!({ "status": status });
        if let Some(err) = error {
            payload["error"] = err;
        }
        let finished = self.prepare(
            NewEvent::new(session, kinds::AGENT_FINISHED)
                .agent(id.clone())
                .payload(payload),
        )?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n = tx.execute(
            "UPDATE agents SET status = ?2, ended_at = ?3 WHERE id = ?1",
            params![id, status.as_str(), now_ts()],
        )?;
        require_row(n, "agent", id.as_str())?;
        tx.execute(
            "UPDATE sessions SET last_agent_status = ?2, updated_at = ?3 WHERE id = ?1",
            params![session_for_status, status.as_str(), now_ts()],
        )?;
        insert_event(&tx, finished)?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_agent(&self, id: &AgentId) -> Result<AgentRecord> {
        self.lock()
            .query_row(
                "SELECT id, session_id, parent_agent_id, kind, created_at, ended_at, status, task_text, options_json
                 FROM agents WHERE id = ?1",
                params![id],
                agent_record,
            )
            .optional()?
            .ok_or_else(|| TraceError::not_found("agent", id.as_str()))
    }

    pub fn list_agents(&self, session: &SessionId) -> Result<Vec<AgentRecord>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT id, session_id, parent_agent_id, kind, created_at, ended_at, status, task_text, options_json
             FROM agents WHERE session_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![session], agent_record)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // --------------------------------------------------------------- steps

    pub fn start_step(&self, agent: &AgentId) -> Result<StepId> {
        let id = StepId::generate();
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT COUNT(*) > 0 FROM agents WHERE id = ?1",
            params![agent],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(TraceError::not_found("agent", agent.as_str()));
        }
        tx.execute(
            "INSERT INTO steps(id, agent_id, seq, started_at, status)
             VALUES (?1, ?2, (SELECT COALESCE(MAX(seq), 0) + 1 FROM steps WHERE agent_id = ?2), ?3, 'running')",
            params![id, agent, now_ts()],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn finish_step(&self, id: &StepId, status: RunStatus) -> Result<()> {
        let n = self.lock().execute(
            "UPDATE steps SET status = ?2, ended_at = ?3 WHERE id = ?1",
            params![id, status.as_str(), now_ts()],
        )?;
        require_row(n, "step", id.as_str())
    }

    pub fn get_step(&self, id: &StepId) -> Result<StepRecord> {
        self.lock()
            .query_row(
                "SELECT id, agent_id, seq, started_at, ended_at, status FROM steps WHERE id = ?1",
                params![id],
                step_record,
            )
            .optional()?
            .ok_or_else(|| TraceError::not_found("step", id.as_str()))
    }

    pub fn list_steps(&self, agent: &AgentId) -> Result<Vec<StepRecord>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT id, agent_id, seq, started_at, ended_at, status FROM steps
             WHERE agent_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![agent], step_record)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // -------------------------------------------------------------- events

    /// Appends one event, assigning the next per-session `seq` atomically.
    /// Strings above the inline limit and the attached blob go to the blob
    /// store first (outside the lock).
    pub fn append(&self, ev: NewEvent) -> Result<EventId> {
        let prepared = self.prepare(ev)?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = insert_event(&tx, prepared)?;
        tx.commit()?;
        Ok(id)
    }

    /// Appends several events of one logical action in a single
    /// transaction (all or nothing, consecutive `seq`).
    pub fn append_all(&self, evs: Vec<NewEvent>) -> Result<Vec<EventId>> {
        let prepared = evs
            .into_iter()
            .map(|ev| self.prepare(ev))
            .collect::<Result<Vec<_>>>()?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = prepared
            .into_iter()
            .map(|p| insert_event(&tx, p))
            .collect::<Result<Vec<_>>>()?;
        tx.commit()?;
        Ok(ids)
    }

    /// Appends many independent events in one transaction, each in its own
    /// savepoint: a failing event is rolled back alone and reported in its
    /// slot. Used by [`super::TraceWriter`] to amortise commit cost.
    pub fn append_batch(&self, evs: Vec<NewEvent>) -> Result<Vec<Result<EventId>>> {
        let prepared: Vec<Result<Prepared>> = evs.into_iter().map(|ev| self.prepare(ev)).collect();
        let mut conn = self.lock();
        let mut tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut out = Vec::with_capacity(prepared.len());
        for p in prepared {
            let result = match p {
                Err(e) => Err(e),
                Ok(p) => {
                    let sp = tx.savepoint()?;
                    match insert_event(&sp, p) {
                        Ok(id) => {
                            sp.commit()?;
                            Ok(id)
                        }
                        Err(e) => Err(e), // dropping `sp` rolls it back
                    }
                }
            };
            out.push(result);
        }
        tx.commit()?;
        Ok(out)
    }

    /// Ascending by `seq` (see [`EventQuery`]).
    pub fn list_events(&self, q: &EventQuery) -> Result<Vec<EventSummary>> {
        let (where_sql, args) = event_where(q);
        let sql = format!(
            "SELECT e.id, e.session_id, e.agent_id, e.step_id, e.seq, e.ts, e.kind, b.size
             FROM events e LEFT JOIN blobs b ON b.id = e.blob_id
             WHERE {where_sql}
             ORDER BY e.session_id {order}, e.seq {order} LIMIT ?{n}",
            order = if q.before_seq.is_some() {
                "DESC"
            } else {
                "ASC"
            },
            n = args.len() + 1,
        );
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&sql)?;
        let mut args = args;
        args.push(rusqlite::types::Value::Integer(i64::from(page(q.limit))));
        let mut rows: Vec<EventSummary> = stmt
            .query_map(rusqlite::params_from_iter(args), event_summary)?
            .collect::<rusqlite::Result<_>>()?;
        if q.before_seq.is_some() {
            rows.reverse();
        }
        Ok(rows)
    }

    pub fn get_event(&self, id: &EventId) -> Result<TraceEvent> {
        self.lock()
            .query_row(
                "SELECT e.id, e.session_id, e.agent_id, e.step_id, e.seq, e.ts, e.kind, b.size,
                        e.payload_json, e.blob_id
                 FROM events e LEFT JOIN blobs b ON b.id = e.blob_id WHERE e.id = ?1",
                params![id],
                trace_event,
            )
            .optional()?
            .ok_or_else(|| TraceError::not_found("event", id.as_str()))
    }

    /// Every event of a session in order, with payloads (export hook).
    pub fn session_events(&self, session: &SessionId) -> Result<Vec<TraceEvent>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT e.id, e.session_id, e.agent_id, e.step_id, e.seq, e.ts, e.kind, b.size,
                    e.payload_json, e.blob_id
             FROM events e LEFT JOIN blobs b ON b.id = e.blob_id
             WHERE e.session_id = ?1 ORDER BY e.seq",
        )?;
        let rows = stmt.query_map(params![session], trace_event)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Number of events in a session (== last `seq`).
    pub fn event_count(&self, session: &SessionId) -> Result<u64> {
        let n: i64 = self.lock().query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id = ?1",
            params![session],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    // --------------------------------------------------------------- blobs

    /// Stores `bytes` (deduplicated by hash; an existing blob's refcount is
    /// incremented) and returns its id.
    pub fn put_blob(&self, bytes: &[u8], media_type: &str) -> Result<BlobId> {
        let (id, _) = self.files.write(bytes)?;
        let id = BlobId::from(id);
        let conn = self.lock();
        register_blob(&conn, &id, bytes.len(), media_type)?;
        Ok(id)
    }

    pub fn read_blob(&self, id: &BlobId) -> Result<Vec<u8>> {
        self.files.read(id.as_str())
    }

    pub fn blob_meta(&self, id: &BlobId) -> Result<BlobMeta> {
        self.lock()
            .query_row(
                "SELECT id, size, media_type, created_at, refcount, pruned_at FROM blobs WHERE id = ?1",
                params![id],
                |r| {
                    Ok(BlobMeta {
                        id: r.get(0)?,
                        size: get_u64(r, 1)?,
                        media_type: r.get(2)?,
                        created_at: r.get(3)?,
                        refcount: get_u64(r, 4)?,
                        pruned_at: r.get(5)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| TraceError::not_found("blob", id.as_str()))
    }

    /// Retention hook: deletes the blob file but keeps the row (marked
    /// `pruned_at`) so events stay consistent.
    pub fn prune_blob(&self, id: &BlobId) -> Result<()> {
        let n = self.lock().execute(
            "UPDATE blobs SET pruned_at = ?2 WHERE id = ?1 AND pruned_at IS NULL",
            params![id, now_ts()],
        )?;
        require_row(n, "blob", id.as_str())?;
        self.files.remove(id.as_str())
    }

    // -------------------------------------------------------- mentor calls

    pub fn record_mentor_call(&self, c: &MentorCallStart) -> Result<()> {
        let conn = self.lock();
        insert_mentor_call(&conn, c)?;
        Ok(())
    }

    pub fn complete_mentor_call(&self, c: &MentorCallEnd) -> Result<()> {
        let conn = self.lock();
        update_mentor_call(&conn, c)
    }

    pub fn get_mentor_call(&self, id: &CallId) -> Result<MentorCallRow> {
        self.lock()
            .query_row(
                &format!("{MENTOR_CALL_COLUMNS} WHERE id = ?1"),
                params![id],
                mentor_call_row,
            )
            .optional()?
            .ok_or_else(|| TraceError::not_found("mentor call", id.as_str()))
    }

    /// Chronological.
    pub fn list_mentor_calls(&self, f: &CallFilter) -> Result<Vec<MentorCallRow>> {
        let (where_sql, args) = call_where(f);
        let sql = format!("{MENTOR_CALL_COLUMNS} WHERE {where_sql} ORDER BY started_at, id");
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args), mentor_call_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// A page of matching calls, newest first, with the session's title
    /// and workspace, and the number of matching calls in all.
    pub fn list_calls_page(
        &self,
        f: &CallFilter,
        limit: u32,
        offset: u64,
    ) -> Result<(Vec<CallListRow>, u64)> {
        let (where_sql, args) = call_where(f);
        let conn = self.lock();
        let total: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM mentor_calls c WHERE {where_sql}"),
            rusqlite::params_from_iter(args.iter()),
            |r| r.get(0),
        )?;
        let sql = format!(
            "SELECT {}, s.title, s.workspace_id
             FROM mentor_calls c LEFT JOIN sessions s ON s.id = c.session_id
             WHERE {where_sql} ORDER BY c.started_at DESC, c.id DESC
             LIMIT {limit} OFFSET {offset}",
            mentor_call_fields!()
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
            Ok(CallListRow {
                call: mentor_call_row(r)?,
                session_title: r.get(23)?,
                workspace_id: r.get(24)?,
            })
        })?;
        Ok((
            rows.collect::<rusqlite::Result<_>>()?,
            u64::try_from(total).unwrap_or(0),
        ))
    }

    /// Token and cost sums over matching calls.
    pub fn stats(&self, f: &CallFilter) -> Result<UsageTotals> {
        let (where_sql, args) = call_where(f);
        let sql = format!(
            "SELECT COUNT(*), COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0), COALESCE(SUM(cache_creation_tokens), 0),
                    COALESCE(SUM(cost_micros), 0),
                    COALESCE(SUM(CASE WHEN status = 'ok' AND cost_micros IS NULL THEN 1 ELSE 0 END), 0)
             FROM mentor_calls c WHERE {where_sql}"
        );
        let conn = self.lock();
        Ok(conn.query_row(&sql, rusqlite::params_from_iter(args), |r| {
            Ok(UsageTotals {
                calls: get_u64(r, 0)?,
                input_tokens: get_u64(r, 1)?,
                output_tokens: get_u64(r, 2)?,
                cache_read_tokens: get_u64(r, 3)?,
                cache_creation_tokens: get_u64(r, 4)?,
                cost_micros: r.get(5)?,
                unpriced_calls: get_u64(r, 6)?,
            })
        })?)
    }

    /// Token and cost sums per group. Model, day and kind groups are
    /// ordered by key; session and workspace groups by cost (highest
    /// first), then key.
    pub fn stats_by(&self, f: &CallFilter, group: GroupBy) -> Result<Vec<GroupedTotals>> {
        let (where_sql, args) = call_where(f);
        let (key_expr, label_expr, from, order) = match group {
            GroupBy::Model => ("c.model", "NULL", "mentor_calls c", "1"),
            GroupBy::Kind => ("c.kind", "NULL", "mentor_calls c", "1"),
            GroupBy::Workspace => (
                "COALESCE(s.workspace_id, '')",
                "COALESCE(w.root, s.workspace_path)",
                "mentor_calls c LEFT JOIN sessions s ON s.id = c.session_id
                 LEFT JOIN workspaces w ON w.id = s.workspace_id",
                "6 DESC, 1",
            ),
            GroupBy::Day { offset_secs } => {
                // `date()` accepts the `...Z` timestamps written by `now_ts`.
                let expr = format!("date(c.started_at, '{offset_secs:+} seconds')");
                return self.grouped(&where_sql, args, &expr, "NULL", "mentor_calls c", "1");
            }
            GroupBy::Session => (
                "c.session_id",
                "s.title",
                "mentor_calls c LEFT JOIN sessions s ON s.id = c.session_id",
                "6 DESC, 1",
            ),
        };
        self.grouped(&where_sql, args, key_expr, label_expr, from, order)
    }

    fn grouped(
        &self,
        where_sql: &str,
        args: SqlArgs,
        key_expr: &str,
        label_expr: &str,
        from: &str,
        order: &str,
    ) -> Result<Vec<GroupedTotals>> {
        let sql = format!(
            "SELECT {key_expr} AS k, {label_expr},
                    COUNT(*), COALESCE(SUM(c.input_tokens), 0), COALESCE(SUM(c.output_tokens), 0),
                    COALESCE(SUM(c.cost_micros), 0),
                    COALESCE(SUM(c.cache_read_tokens), 0), COALESCE(SUM(c.cache_creation_tokens), 0),
                    COALESCE(SUM(CASE WHEN c.status = 'ok' AND c.cost_micros IS NULL THEN 1 ELSE 0 END), 0)
             FROM {from} WHERE {where_sql} GROUP BY k ORDER BY {order}"
        );
        let conn = self.lock();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
            Ok(GroupedTotals {
                key: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                label: r.get(1)?,
                totals: UsageTotals {
                    calls: get_u64(r, 2)?,
                    input_tokens: get_u64(r, 3)?,
                    output_tokens: get_u64(r, 4)?,
                    cost_micros: r.get(5)?,
                    cache_read_tokens: get_u64(r, 6)?,
                    cache_creation_tokens: get_u64(r, 7)?,
                    unpriced_calls: get_u64(r, 8)?,
                },
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Current offset of the OS local timezone from UTC, in seconds (via
    /// SQLite's `localtime`, which follows the OS rules including DST).
    pub fn local_offset_secs(&self) -> Result<i32> {
        let secs: i64 = self.lock().query_row(
            "SELECT CAST(strftime('%s', 'now', 'localtime') AS INTEGER) - CAST(strftime('%s', 'now') AS INTEGER)",
            [],
            |r| r.get(0),
        )?;
        Ok(i32::try_from(secs).unwrap_or(0))
    }

    /// Recomputes `cost_micros` of every completed call with usage that
    /// matches `f`, using `price(model, usage)`; rows already holding the
    /// recomputed value are left untouched, so a second run changes nothing.
    pub fn reprice(
        &self,
        f: &CallFilter,
        price: &dyn Fn(&str, &Usage) -> Option<i64>,
    ) -> Result<RepriceReport> {
        let (where_sql, args) = call_where(f);
        let sql = format!(
            "SELECT id, model, input_tokens, output_tokens, cache_read_tokens,
                    cache_creation_tokens, cost_micros
             FROM mentor_calls c WHERE {where_sql} AND input_tokens IS NOT NULL"
        );
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut report = RepriceReport::default();
        let updates: Vec<(CallId, Option<i64>)> = {
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
                let usage = Usage {
                    input_tokens: get_u64(r, 2)?,
                    output_tokens: get_u64(r, 3)?,
                    cache_read_input_tokens: get_u64(r, 4)?,
                    cache_creation_input_tokens: get_u64(r, 5)?,
                };
                Ok((
                    r.get::<_, CallId>(0)?,
                    r.get::<_, String>(1)?,
                    usage,
                    r.get::<_, Option<i64>>(6)?,
                ))
            })?;
            let mut updates = Vec::new();
            for row in rows {
                let (id, model, usage, stored) = row?;
                report.examined += 1;
                let fresh = price(&model, &usage);
                if fresh.is_none() {
                    report.unpriced += 1;
                }
                if fresh != stored {
                    updates.push((id, fresh));
                }
            }
            updates
        };
        {
            let mut stmt = tx.prepare("UPDATE mentor_calls SET cost_micros = ?2 WHERE id = ?1")?;
            for (id, cost) in &updates {
                stmt.execute(params![id, cost])?;
                report.changed += 1;
            }
        }
        tx.commit()?;
        Ok(report)
    }

    // ---------------------------------------------------------- workspaces

    /// Registers `root` (already canonical; see `workspace::Workspace`)
    /// or returns the existing row for it, touching `last_used_at`.
    /// `name` is only used for a new row; `None` means the root's file
    /// name.
    pub fn add_workspace(&self, root: &str, name: Option<&str>) -> Result<WorkspaceRecord> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_ts();
        let existing: Option<WorkspaceId> = tx
            .query_row(
                "SELECT id FROM workspaces WHERE root = ?1",
                params![root],
                |r| r.get(0),
            )
            .optional()?;
        let id = if let Some(id) = existing {
            tx.execute(
                "UPDATE workspaces SET last_used_at = ?2 WHERE id = ?1",
                params![id, now],
            )?;
            id
        } else {
            let id = WorkspaceId::generate();
            let name = name
                .map(str::to_owned)
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| default_workspace_name(root));
            tx.execute(
                "INSERT INTO workspaces(id, root, name, created_at, last_used_at, settings_json)
                 VALUES (?1, ?2, ?3, ?4, ?4, '{}')",
                params![id, root, name, now],
            )?;
            id
        };
        let record = tx.query_row(
            "SELECT id, root, name, created_at, last_used_at, settings_json
             FROM workspaces WHERE id = ?1",
            params![id],
            workspace_record,
        )?;
        tx.commit()?;
        Ok(record)
    }

    pub fn get_workspace(&self, id: &WorkspaceId) -> Result<WorkspaceRecord> {
        self.lock()
            .query_row(
                "SELECT id, root, name, created_at, last_used_at, settings_json
                 FROM workspaces WHERE id = ?1",
                params![id],
                workspace_record,
            )
            .optional()?
            .ok_or_else(|| TraceError::not_found("workspace", id.as_str()))
    }

    /// The row whose root is exactly `root` (canonical form).
    pub fn find_workspace(&self, root: &str) -> Result<Option<WorkspaceRecord>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT id, root, name, created_at, last_used_at, settings_json
                 FROM workspaces WHERE root = ?1",
                params![root],
                workspace_record,
            )
            .optional()?)
    }

    /// Most recently used first.
    pub fn list_workspaces(&self) -> Result<Vec<WorkspaceRecord>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT id, root, name, created_at, last_used_at, settings_json
             FROM workspaces ORDER BY last_used_at DESC, id DESC",
        )?;
        let rows = stmt.query_map([], workspace_record)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn touch_workspace(&self, id: &WorkspaceId) -> Result<()> {
        let n = self.lock().execute(
            "UPDATE workspaces SET last_used_at = ?2 WHERE id = ?1",
            params![id, now_ts()],
        )?;
        require_row(n, "workspace", id.as_str())
    }

    pub fn rename_workspace(&self, id: &WorkspaceId, name: &str) -> Result<()> {
        let n = self.lock().execute(
            "UPDATE workspaces SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        require_row(n, "workspace", id.as_str())
    }

    pub fn set_workspace_settings(&self, id: &WorkspaceId, settings: &Value) -> Result<()> {
        let n = self.lock().execute(
            "UPDATE workspaces SET settings_json = ?2 WHERE id = ?1",
            params![id, settings.to_string()],
        )?;
        require_row(n, "workspace", id.as_str())
    }

    /// Removes the registry row. Sessions keep their `workspace_path`
    /// and lose the link; files and traces are untouched. Returns the
    /// number of sessions unlinked.
    pub fn remove_workspace(&self, id: &WorkspaceId) -> Result<u64> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let unlinked = tx.execute(
            "UPDATE sessions SET workspace_id = NULL WHERE workspace_id = ?1",
            params![id],
        )?;
        let n = tx.execute("DELETE FROM workspaces WHERE id = ?1", params![id])?;
        require_row(n, "workspace", id.as_str())?;
        tx.commit()?;
        Ok(unlinked as u64)
    }

    // ------------------------------------------------------- maintenance

    /// Verifies the database and every blob: files exist (unless pruned)
    /// and hash to their id; payload references resolve; every
    /// `mentor.request` carries its body.
    pub fn integrity_check(&self) -> Result<IntegrityReport> {
        let mut report = IntegrityReport::default();
        let (blob_ids, events): (Vec<(BlobId, bool)>, Vec<EventRow>) = {
            let conn = self.lock();
            report.sqlite = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
            let mut stmt =
                conn.prepare("SELECT id, pruned_at IS NOT NULL FROM blobs ORDER BY id")?;
            let blobs = stmt
                .query_map([], |r| Ok((r.get::<_, BlobId>(0)?, r.get::<_, bool>(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut stmt = conn.prepare(
                "SELECT id, kind, payload_json, blob_id FROM events ORDER BY session_id, seq",
            )?;
            let events = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<_>>()?;
            (blobs, events)
        };
        let known: std::collections::HashSet<&str> =
            blob_ids.iter().map(|(id, _)| id.as_str()).collect();
        for (id, pruned) in &blob_ids {
            if *pruned {
                continue;
            }
            report.blobs_checked += 1;
            match self.files.verify(id.as_str()) {
                Ok(()) => {}
                Err(TraceError::NotFound { .. }) => report.missing_blobs.push(id.clone()),
                Err(TraceError::BlobCorrupted { .. }) => report.corrupted_blobs.push(id.clone()),
                Err(e) => return Err(e),
            }
        }
        for (event_id, kind, payload, blob_id) in events {
            if kind == kinds::MENTOR_REQUEST && blob_id.is_none() {
                report.requests_without_blob.push(event_id.clone());
            }
            let payload: Value = serde_json::from_str(&payload)?;
            for r in blob_refs(&payload) {
                if !known.contains(r.as_str()) {
                    report.dangling_refs.push((event_id.clone(), r));
                }
            }
        }
        Ok(report)
    }

    pub fn disk_usage(&self) -> Result<DiskUsage> {
        let file_len = |p: PathBuf| std::fs::metadata(p).map_or(0, |m| m.len());
        let mut wal = self.db_path.clone().into_os_string();
        wal.push("-wal");
        let (blob_bytes, blob_files) = self.files.disk_usage()?;
        let conn = self.lock();
        let count = |table: &str| -> Result<u64> {
            let n: i64 =
                conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
            Ok(u64::try_from(n).unwrap_or(0))
        };
        Ok(DiskUsage {
            database_bytes: file_len(self.db_path.clone()),
            wal_bytes: file_len(PathBuf::from(wal)),
            blob_bytes,
            blob_files,
            sessions: count("sessions")?,
            events: count("events")?,
            blob_rows: count("blobs")?,
        })
    }

    /// Flushes the WAL into the main file.
    pub fn checkpoint(&self) -> Result<()> {
        self.lock()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    }

    // ------------------------------------------------------------ internal

    /// Validates the kind and moves large data to blob files. No lock held.
    pub(super) fn prepare(&self, mut ev: NewEvent) -> Result<Prepared> {
        if !kinds::is_valid(&ev.kind) {
            return Err(TraceError::invalid(
                "event kind",
                format!("`{}` (expected `area.name` in lower-case)", ev.kind),
            ));
        }
        let mut register = Vec::new();
        let files = &self.files;
        spill_strings(&mut ev.payload, self.inline_max, &mut |s| {
            let (id, _) = files.write(s.as_bytes())?;
            let id = BlobId::from(id);
            register.push((id.clone(), s.len(), SPILLED_MEDIA_TYPE.to_owned()));
            Ok(id)
        })?;
        let mut blob_id = None;
        let mut reference = None;
        match ev.blob.take() {
            Some(BlobInput::Bytes { data, media_type }) => {
                let (id, _) = files.write(&data)?;
                let id = BlobId::from(id);
                register.push((id.clone(), data.len(), media_type));
                blob_id = Some(id);
            }
            Some(BlobInput::Existing(id)) => {
                blob_id = Some(id.clone());
                reference = Some(id);
            }
            None => {}
        }
        Ok(Prepared {
            ev,
            blob_id,
            register,
            reference,
        })
    }

    /// Inserts an event and a `mentor_calls` row pointing at it in one
    /// transaction (`call.request_event` is overwritten).
    pub(super) fn append_with_call(&self, ev: NewEvent, call: &MentorCallStart) -> Result<EventId> {
        let prepared = self.prepare(ev)?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = insert_event(&tx, prepared)?;
        let call = MentorCallStart {
            request_event: id.clone(),
            ..call.clone()
        };
        insert_mentor_call(&tx, &call)?;
        tx.commit()?;
        Ok(id)
    }

    /// Inserts prepared events and completes a `mentor_calls` row in one
    /// transaction.
    /// With `link_first`, the first event becomes `response_event_id`.
    pub(super) fn append_with_completion(
        &self,
        evs: Vec<NewEvent>,
        end: &MentorCallEnd,
        link_first: bool,
    ) -> Result<Vec<EventId>> {
        let prepared = evs
            .into_iter()
            .map(|ev| self.prepare(ev))
            .collect::<Result<Vec<_>>>()?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = prepared
            .into_iter()
            .map(|p| insert_event(&tx, p))
            .collect::<Result<Vec<_>>>()?;
        let end = MentorCallEnd {
            response_event: if link_first {
                ids.first().cloned()
            } else {
                end.response_event.clone()
            },
            ..end.clone()
        };
        update_mentor_call(&tx, &end)?;
        tx.commit()?;
        Ok(ids)
    }
}

// ------------------------------------------------------------ SQL helpers

/// The last path component of `root`, or the whole thing for a bare
/// drive or `/`.
fn default_workspace_name(root: &str) -> String {
    root.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|n| !n.is_empty())
        .unwrap_or(root)
        .to_owned()
}

pub(super) fn page(limit: Option<u32>) -> u32 {
    limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE)
}

pub(super) fn require_row(n: usize, what: &'static str, id: &str) -> Result<()> {
    if n == 0 {
        Err(TraceError::not_found(what, id))
    } else {
        Ok(())
    }
}

pub(super) fn get_u64(r: &Row<'_>, idx: usize) -> rusqlite::Result<u64> {
    let v: i64 = r.get(idx)?;
    Ok(u64::try_from(v).unwrap_or(0))
}

fn get_opt_u64(r: &Row<'_>, idx: usize) -> rusqlite::Result<Option<u64>> {
    let v: Option<i64> = r.get(idx)?;
    Ok(v.map(|v| u64::try_from(v).unwrap_or(0)))
}

pub(super) fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn size_i64(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

pub(super) fn parse_status(s: &str) -> rusqlite::Result<RunStatus> {
    RunStatus::parse(s).ok_or_else(|| bad_column("status", s))
}

pub(super) fn bad_column(column: &str, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        format!("invalid {column} `{value}`").into(),
    )
}

pub(super) fn parse_json(s: &str) -> rusqlite::Result<Value> {
    serde_json::from_str(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

pub(super) const SESSION_COLUMNS: &str =
    "SELECT id, created_at, updated_at, title, workspace_path, config_json, status, workspace_id,
            title_source, message_count, last_agent_status, prompt_version, tools_hash
     FROM sessions";

pub(super) fn session_record(r: &Row<'_>) -> rusqlite::Result<SessionRecord> {
    let status: String = r.get(6)?;
    let title_source: Option<String> = r.get(8)?;
    let last_status: Option<String> = r.get(10)?;
    Ok(SessionRecord {
        id: r.get(0)?,
        created_at: r.get(1)?,
        updated_at: r.get(2)?,
        title: r.get(3)?,
        title_source: title_source
            .map(|s| TitleSource::parse(&s).ok_or_else(|| bad_column("title_source", &s)))
            .transpose()?,
        workspace_path: r.get(4)?,
        workspace_id: r.get(7)?,
        config: parse_json(&r.get::<_, String>(5)?)?,
        status: SessionStatus::parse(&status).ok_or_else(|| bad_column("status", &status))?,
        message_count: get_u64(r, 9)?,
        last_agent_status: last_status.map(|s| parse_status(&s)).transpose()?,
        prompt_version: r.get(11)?,
        tools_hash: r.get(12)?,
    })
}

fn workspace_record(r: &Row<'_>) -> rusqlite::Result<WorkspaceRecord> {
    Ok(WorkspaceRecord {
        id: r.get(0)?,
        root: r.get(1)?,
        name: r.get(2)?,
        created_at: r.get(3)?,
        last_used_at: r.get(4)?,
        settings: parse_json(&r.get::<_, String>(5)?)?,
    })
}

fn agent_record(r: &Row<'_>) -> rusqlite::Result<AgentRecord> {
    let kind: String = r.get(3)?;
    let status: String = r.get(6)?;
    Ok(AgentRecord {
        id: r.get(0)?,
        session_id: r.get(1)?,
        parent_agent_id: r.get(2)?,
        kind: AgentKind::parse(&kind).ok_or_else(|| bad_column("kind", &kind))?,
        created_at: r.get(4)?,
        ended_at: r.get(5)?,
        status: parse_status(&status)?,
        task_text: r.get(7)?,
        options: parse_json(&r.get::<_, String>(8)?)?,
    })
}

fn step_record(r: &Row<'_>) -> rusqlite::Result<StepRecord> {
    let status: String = r.get(5)?;
    Ok(StepRecord {
        id: r.get(0)?,
        agent_id: r.get(1)?,
        seq: get_u64(r, 2)?,
        started_at: r.get(3)?,
        ended_at: r.get(4)?,
        status: parse_status(&status)?,
    })
}

fn event_summary(r: &Row<'_>) -> rusqlite::Result<EventSummary> {
    Ok(EventSummary {
        id: r.get(0)?,
        session_id: r.get(1)?,
        agent_id: r.get(2)?,
        step_id: r.get(3)?,
        seq: get_u64(r, 4)?,
        ts: r.get(5)?,
        kind: r.get(6)?,
        blob_bytes: get_opt_u64(r, 7)?,
    })
}

fn trace_event(r: &Row<'_>) -> rusqlite::Result<TraceEvent> {
    Ok(TraceEvent {
        summary: event_summary(r)?,
        payload: parse_json(&r.get::<_, String>(8)?)?,
        blob_id: r.get(9)?,
    })
}

/// The columns [`mentor_call_row`] reads, qualified so a join can add
/// its own after them.
macro_rules! mentor_call_fields {
    () => {
        "c.id, c.session_id, c.agent_id, c.step_id, c.request_event_id, c.response_event_id,
         c.model, c.effort, c.started_at, c.ended_at, c.status, c.stop_reason, c.http_status,
         c.input_tokens, c.output_tokens, c.cache_read_tokens, c.cache_creation_tokens,
         c.cost_micros, c.first_byte_ms, c.total_ms, c.request_bytes, c.apprentice_applied, c.kind"
    };
}
pub(super) use mentor_call_fields;

pub(super) const MENTOR_CALL_COLUMNS: &str =
    concat!("SELECT ", mentor_call_fields!(), " FROM mentor_calls c");

pub(super) fn mentor_call_row(r: &Row<'_>) -> rusqlite::Result<MentorCallRow> {
    let status: String = r.get(10)?;
    let kind: String = r.get(22)?;
    let input: Option<i64> = r.get(13)?;
    let usage = input.map(|_| {
        Ok::<_, rusqlite::Error>(Usage {
            input_tokens: get_u64(r, 13)?,
            output_tokens: get_u64(r, 14)?,
            cache_read_input_tokens: get_u64(r, 15)?,
            cache_creation_input_tokens: get_u64(r, 16)?,
        })
    });
    Ok(MentorCallRow {
        id: r.get(0)?,
        session_id: r.get(1)?,
        agent_id: r.get(2)?,
        step_id: r.get(3)?,
        request_event_id: r.get(4)?,
        response_event_id: r.get(5)?,
        model: r.get(6)?,
        effort: r.get(7)?,
        started_at: r.get(8)?,
        ended_at: r.get(9)?,
        status: parse_status(&status)?,
        stop_reason: r.get(11)?,
        http_status: r
            .get::<_, Option<i64>>(12)?
            .map(|v| u16::try_from(v).unwrap_or(0)),
        usage: usage.transpose()?,
        cost_micros: r.get(17)?,
        first_byte_ms: get_opt_u64(r, 18)?,
        total_ms: get_opt_u64(r, 19)?,
        request_bytes: get_opt_u64(r, 20)?,
        apprentice_applied: r.get::<_, i64>(21)? != 0,
        kind: CallKind::parse(&kind).ok_or_else(|| bad_column("kind", &kind))?,
    })
}

/// Inserts the blob row or bumps its refcount.
fn register_blob(conn: &Connection, id: &BlobId, size: usize, media_type: &str) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO blobs(id, size, media_type, created_at, refcount) VALUES (?1, ?2, ?3, ?4, 1)
         ON CONFLICT(id) DO UPDATE SET refcount = refcount + 1",
    )?
    .execute(params![id, size_i64(size), media_type, now_ts()])?;
    Ok(())
}

pub(super) fn insert_event(conn: &Connection, p: Prepared) -> Result<EventId> {
    let Prepared {
        ev,
        blob_id,
        register,
        reference,
    } = p;
    for (id, size, media_type) in &register {
        register_blob(conn, id, *size, media_type)?;
    }
    if let Some(id) = &reference {
        let n = conn
            .prepare_cached("UPDATE blobs SET refcount = refcount + 1 WHERE id = ?1")?
            .execute(params![id])?;
        require_row(n, "blob", id.as_str())?;
    }
    let exists: bool = conn
        .prepare_cached("SELECT COUNT(*) > 0 FROM sessions WHERE id = ?1")?
        .query_row(params![ev.session], |r| r.get(0))?;
    if !exists {
        return Err(TraceError::not_found("session", ev.session.as_str()));
    }
    let id = EventId::generate();
    let now = now_ts();
    conn.prepare_cached(
        "INSERT INTO events(id, session_id, agent_id, step_id, seq, ts, kind, payload_json, blob_id)
         VALUES (?1, ?2, ?3, ?4,
                 (SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE session_id = ?2),
                 ?5, ?6, ?7, ?8)",
    )?
    .execute(params![
        id,
        ev.session,
        ev.agent,
        ev.step,
        now,
        ev.kind,
        ev.payload.to_string(),
        blob_id
    ])?;
    conn.prepare_cached("UPDATE sessions SET updated_at = ?2 WHERE id = ?1")?
        .execute(params![ev.session, now])?;
    Ok(id)
}

pub(super) fn insert_mentor_call(conn: &Connection, c: &MentorCallStart) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO mentor_calls(id, session_id, agent_id, step_id, request_event_id, model, effort,
                                  started_at, status, request_bytes, kind)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'running', ?9, ?10)",
    )?
    .execute(params![
        c.id,
        c.session,
        c.agent,
        c.step,
        c.request_event,
        c.model,
        c.effort,
        c.started_at.clone().unwrap_or_else(now_ts),
        c.request_bytes.map(to_i64),
        c.kind.as_str(),
    ])?;
    Ok(())
}

pub(super) fn update_mentor_call(conn: &Connection, c: &MentorCallEnd) -> Result<()> {
    let n = conn
        .prepare_cached(
            "UPDATE mentor_calls SET response_event_id = ?2, status = ?3, ended_at = ?4,
                    stop_reason = ?5, http_status = ?6,
                    input_tokens = ?7, output_tokens = ?8, cache_read_tokens = ?9, cache_creation_tokens = ?10,
                    cost_micros = ?11, first_byte_ms = ?12, total_ms = ?13
             WHERE id = ?1",
        )?
        .execute(params![
            c.id,
            c.response_event,
            c.status.as_str(),
            now_ts(),
            c.stop_reason,
            c.http_status.map(i64::from),
            c.usage.map(|u| to_i64(u.input_tokens)),
            c.usage.map(|u| to_i64(u.output_tokens)),
            c.usage.map(|u| to_i64(u.cache_read_input_tokens)),
            c.usage.map(|u| to_i64(u.cache_creation_input_tokens)),
            c.cost_micros,
            c.first_byte_ms.map(to_i64),
            c.total_ms.map(to_i64),
        ])?;
    require_row(n, "mentor call", c.id.as_str())
}

pub(super) type SqlArgs = Vec<rusqlite::types::Value>;
/// `(id, kind, payload_json, blob_id)` as scanned by `integrity_check`.
type EventRow = (EventId, String, String, Option<BlobId>);

fn event_where(q: &EventQuery) -> (String, SqlArgs) {
    let mut clauses = Vec::new();
    let mut args: SqlArgs = Vec::new();
    let mut bind = |clause: &str, v: rusqlite::types::Value| {
        args.push(v);
        clauses.push(clause.replace('?', &format!("?{}", args.len())));
    };
    if let Some(s) = &q.session_id {
        bind("e.session_id = ?", s.as_str().to_owned().into());
    }
    if let Some(a) = &q.agent_id {
        bind("e.agent_id = ?", a.as_str().to_owned().into());
    }
    if let Some(s) = &q.step_id {
        bind("e.step_id = ?", s.as_str().to_owned().into());
    }
    if let Some(b) = q.before_seq {
        bind("e.seq < ?", to_i64(b).into());
    }
    if let Some(a) = q.after_seq {
        bind("e.seq > ?", to_i64(a).into());
    }
    if !q.kinds.is_empty() {
        let placeholders: Vec<String> = q
            .kinds
            .iter()
            .map(|k| {
                args.push(k.as_str().to_owned().into());
                format!("?{}", args.len())
            })
            .collect();
        clauses.push(format!("e.kind IN ({})", placeholders.join(", ")));
    }
    if clauses.is_empty() {
        clauses.push("1".to_owned());
    }
    (clauses.join(" AND "), args)
}

/// `WHERE` clause over `mentor_calls c` (the alias lets callers join).
fn call_where(f: &CallFilter) -> (String, SqlArgs) {
    let mut clauses = Vec::new();
    let mut args: SqlArgs = Vec::new();
    let mut bind = |clause: &str, v: rusqlite::types::Value| {
        args.push(v);
        clauses.push(clause.replace('?', &format!("?{}", args.len())));
    };
    if let Some(s) = &f.session_id {
        bind("c.session_id = ?", s.as_str().to_owned().into());
    }
    if let Some(a) = &f.agent_id {
        bind("c.agent_id = ?", a.as_str().to_owned().into());
    }
    if let Some(m) = &f.model {
        bind("c.model = ?", m.as_str().to_owned().into());
    }
    if let Some(s) = f.status {
        bind("c.status = ?", s.as_str().to_owned().into());
    }
    if let Some(s) = &f.since {
        bind("c.started_at >= ?", s.as_str().to_owned().into());
    }
    if let Some(u) = &f.until {
        bind("c.started_at < ?", u.as_str().to_owned().into());
    }
    if let Some(w) = &f.workspace_id {
        bind(
            "c.session_id IN (SELECT id FROM sessions WHERE workspace_id = ?)",
            w.as_str().to_owned().into(),
        );
    }
    if clauses.is_empty() {
        clauses.push("1".to_owned());
    }
    (clauses.join(" AND "), args)
}
