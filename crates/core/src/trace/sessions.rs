//! The materialised conversation of a session (task M01-10): one row of
//! `session_messages` per message, exactly as the mentor sent or
//! received it, mirrored from the in-memory [`crate::runtime::Conversation`]
//! — row `seq` is message index + 1, always. Resume reads these rows,
//! not the events; the trace stays the record. On top: the session list
//! with activity and cost, full-text search, archive/delete, and the
//! JSON export with its import.

use apprentice_api::events::AgentStatus;
use apprentice_api::types::{
    AgentSummary, MentorCallInfo, SESSION_EXPORT_FORMAT, SessionExport, SessionInfo,
    SessionMessage, SessionSearchHit, SessionSummary, Usage,
};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde_json::Value;

use super::error::TraceError;
use super::payload::blob_refs;
use super::store::{
    MENTOR_CALL_COLUMNS, MentorCallStart, NewEvent, TraceStore, bad_column, get_u64, insert_event,
    insert_mentor_call, mentor_call_row, page, parse_json, require_row, to_i64,
};
use super::{
    AgentId, CallKind, EventId, RunStatus, SessionId, SessionStatus, StepId, TitleSource,
    WorkspaceId, kinds, now_ts,
};
use crate::mentor::{ContentBlock, Role};
use crate::stats::{micros_to_usd, usd_to_micros};

type Result<T> = std::result::Result<T, TraceError>;

/// Characters of the first prompt that become the provisional title.
pub const PROMPT_TITLE_CHARS: usize = 60;

/// A message to store at `seq` (replacing what is there: the runtime
/// rewrites the last row when a user turn grows).
#[derive(Debug, Clone)]
pub struct NewMessage {
    pub session: SessionId,
    pub seq: u64,
    pub role: Role,
    /// The blocks as sent or received, cache flags and all (the
    /// runtime stores them without).
    pub content: Vec<ContentBlock>,
    pub agent: Option<AgentId>,
    pub step: Option<StepId>,
}

/// A stored message.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredMessage {
    pub seq: u64,
    pub role: Role,
    pub content: Vec<ContentBlock>,
    pub agent_id: Option<AgentId>,
    pub step_id: Option<StepId>,
    pub created_at: String,
}

impl StoredMessage {
    /// The wire form (`session.get`, exports).
    ///
    /// # Errors
    /// A content block that does not serialise (does not happen).
    pub fn into_wire(self) -> Result<SessionMessage> {
        Ok(SessionMessage {
            seq: self.seq,
            role: role_str(self.role).to_owned(),
            content: serde_json::to_value(&self.content)?,
            agent_id: self.agent_id.map(AgentId::into_string),
            step_id: self.step_id.map(StepId::into_string),
            created_at: self.created_at,
        })
    }
}

/// Filter for [`TraceStore::list_sessions`].
#[derive(Debug, Clone, Default)]
pub struct SessionQuery {
    /// Words the title or a message must contain.
    pub query: Option<String>,
    /// The workspace root the session was created on.
    pub workspace: Option<String>,
    pub workspace_id: Option<WorkspaceId>,
    /// Also archived and deleted sessions.
    pub include_archived: bool,
    pub limit: Option<u32>,
    pub offset: u32,
}

/// What [`TraceStore::delete_session`] removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeleteReport {
    pub messages: u64,
    /// Only with `purge_traces`.
    pub events: u64,
}

impl TraceStore {
    // ------------------------------------------------------------ messages

    /// Stores `m` at its `seq`, replacing an existing row (and its
    /// search text), and refreshes the session's `message_count`.
    pub fn put_session_message(&self, m: &NewMessage) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        put_message(&tx, m)?;
        tx.commit()?;
        Ok(())
    }

    /// Appends `ev` and stores `m` in one transaction (the user turn
    /// with its `user.message`, the assistant turn with its
    /// `assistant.message`).
    pub fn append_with_message(&self, ev: NewEvent, m: &NewMessage) -> Result<EventId> {
        let prepared = self.prepare(ev)?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = insert_event(&tx, prepared)?;
        put_message(&tx, m)?;
        tx.commit()?;
        Ok(id)
    }

    /// The messages of `session` with `seq` above `after_seq`, in
    /// order; all of them when `limit` is `None`.
    pub fn session_messages(
        &self,
        session: &SessionId,
        after_seq: u64,
        limit: Option<u32>,
    ) -> Result<Vec<StoredMessage>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT seq, role, content_json, agent_id, step_id, created_at FROM session_messages
             WHERE session_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let limit = limit.map_or(-1, i64::from);
        let rows = stmt.query_map(params![session, to_i64(after_seq), limit], stored_message)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The newest `limit` messages below `before_seq`, oldest first
    /// (the chat view pages backwards from the end).
    pub fn session_messages_before(
        &self,
        session: &SessionId,
        before_seq: u64,
        limit: u32,
    ) -> Result<Vec<StoredMessage>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT seq, role, content_json, agent_id, step_id, created_at FROM session_messages
             WHERE session_id = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![session, to_i64(before_seq), limit], stored_message)?;
        let mut rows: Vec<StoredMessage> = rows.collect::<rusqlite::Result<_>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// Every agent of the session, oldest first, with the totals of its
    /// mentor calls.
    pub fn session_agents(&self, session: &SessionId) -> Result<Vec<AgentSummary>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT a.id, a.status, a.created_at, a.ended_at,
                    (SELECT model FROM mentor_calls c
                      WHERE c.agent_id = a.id AND c.kind = 'step' ORDER BY started_at LIMIT 1),
                    (SELECT COUNT(*) FROM mentor_calls c WHERE c.agent_id = a.id),
                    (SELECT COALESCE(SUM(input_tokens), 0) FROM mentor_calls c WHERE c.agent_id = a.id),
                    (SELECT COALESCE(SUM(output_tokens), 0) FROM mentor_calls c WHERE c.agent_id = a.id),
                    (SELECT COALESCE(SUM(cache_read_tokens), 0) FROM mentor_calls c WHERE c.agent_id = a.id),
                    (SELECT COALESCE(SUM(cache_creation_tokens), 0) FROM mentor_calls c WHERE c.agent_id = a.id),
                    (SELECT COALESCE(SUM(cost_micros), 0) FROM mentor_calls c WHERE c.agent_id = a.id),
                    (SELECT COUNT(*) FROM mentor_calls c
                      WHERE c.agent_id = a.id AND c.status = 'ok' AND c.cost_micros IS NULL)
             FROM agents a WHERE a.session_id = ?1 ORDER BY a.created_at, a.id",
        )?;
        let rows = stmt.query_map(params![session], |r| {
            let cost_micros: i64 = r.get(10)?;
            let unpriced: i64 = r.get(11)?;
            Ok(AgentSummary {
                id: r.get(0)?,
                status: r.get(1)?,
                started_at: r.get(2)?,
                ended_at: r.get(3)?,
                model: r.get(4)?,
                calls: get_u64(r, 5)?,
                usage: Usage {
                    input_tokens: get_u64(r, 6)?,
                    output_tokens: get_u64(r, 7)?,
                    cache_read_input_tokens: get_u64(r, 8)?,
                    cache_creation_input_tokens: get_u64(r, 9)?,
                },
                cost_usd: (unpriced == 0).then(|| micros_to_usd(cost_micros)),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Drops the rows above `keep` (a trailing assistant turn whose
    /// tool calls were never answered). Returns how many went.
    pub fn truncate_session_messages(&self, session: &SessionId, keep: u64) -> Result<u64> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM session_fts WHERE session_id = ?1 AND seq > ?2",
            params![session, to_i64(keep)],
        )?;
        let n = tx.execute(
            "DELETE FROM session_messages WHERE session_id = ?1 AND seq > ?2",
            params![session, to_i64(keep)],
        )?;
        refresh_count(&tx, session)?;
        tx.commit()?;
        Ok(n as u64)
    }

    // ------------------------------------------------------------ listing

    /// Sessions by last activity, newest first. Open ones only unless
    /// `include_archived`.
    pub fn list_sessions(&self, q: &SessionQuery) -> Result<Vec<SessionSummary>> {
        let mut clauses = vec![if q.include_archived {
            "1".to_owned()
        } else {
            "s.status = 'open'".to_owned()
        }];
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(ws) = &q.workspace {
            args.push(ws.clone().into());
            clauses.push(format!("s.workspace_path = ?{}", args.len()));
        }
        if let Some(id) = &q.workspace_id {
            args.push(id.as_str().to_owned().into());
            clauses.push(format!("s.workspace_id = ?{}", args.len()));
        }
        if let Some(words) = q.query.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            args.push(words.to_lowercase().into());
            let title = format!("instr(lower(COALESCE(s.title, '')), ?{}) > 0", args.len());
            let fts = fts_query(words);
            if fts.is_empty() {
                clauses.push(title);
            } else {
                args.push(fts.into());
                clauses.push(format!(
                    "({title} OR s.id IN (SELECT session_id FROM session_fts WHERE session_fts MATCH ?{}))",
                    args.len()
                ));
            }
        }
        args.push(i64::from(page(q.limit)).into());
        let limit = args.len();
        args.push(i64::from(q.offset).into());
        let sql = format!(
            "{SUMMARY_COLUMNS} WHERE {} ORDER BY last_activity DESC, s.id DESC LIMIT ?{limit} OFFSET ?{}",
            clauses.join(" AND "),
            args.len()
        );
        let conn = self.lock();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
            Ok(session_info(r)?.summary)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The session with its activity, cost and resume metadata.
    pub fn session_info(&self, id: &SessionId) -> Result<SessionInfo> {
        self.lock()
            .query_row(
                &format!("{SUMMARY_COLUMNS} WHERE s.id = ?1"),
                params![id],
                session_info,
            )
            .optional()?
            .ok_or_else(|| TraceError::not_found("session", id.as_str()))
    }

    /// Full-text search over the stored messages, newest hits first.
    /// Every word of `query` must appear (as a prefix); an empty query
    /// finds nothing.
    pub fn search_sessions(
        &self,
        query: &str,
        include_archived: bool,
        limit: Option<u32>,
    ) -> Result<Vec<SessionSearchHit>> {
        let fts = fts_query(query);
        if fts.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT f.session_id, s.title, s.workspace_path, m.seq, m.role,
                    snippet(session_fts, 2, '[', ']', '…', 12), m.created_at
             FROM session_fts f
             JOIN session_messages m ON m.session_id = f.session_id AND m.seq = f.seq
             JOIN sessions s ON s.id = f.session_id
             WHERE session_fts MATCH ?1 AND (s.status = 'open' OR ?2)
             ORDER BY m.created_at DESC, m.seq DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![fts, include_archived, page(limit)], |r| {
            Ok(SessionSearchHit {
                session_id: r.get(0)?,
                title: r.get(1)?,
                workspace: r.get(2)?,
                seq: get_u64(r, 3)?,
                role: r.get(4)?,
                snippet: r.get(5)?,
                created_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---------------------------------------------------------- lifecycle

    /// Archives (or reopens) the session and records `session.status`.
    pub fn archive_session(&self, id: &SessionId, archived: bool) -> Result<()> {
        let status = if archived {
            SessionStatus::Archived
        } else {
            SessionStatus::Open
        };
        let ev = self.prepare(
            NewEvent::new(id.clone(), kinds::SESSION_STATUS)
                .payload(serde_json::json!({ "status": status })),
        )?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n = tx.execute(
            "UPDATE sessions SET status = ?2, updated_at = ?3 WHERE id = ?1 AND status != 'deleted'",
            params![id, status.as_str(), now_ts()],
        )?;
        require_row(n, "session", id.as_str())?;
        insert_event(&tx, ev)?;
        tx.commit()?;
        Ok(())
    }

    /// Removes the stored conversation. Without `purge_traces` the row
    /// stays as `deleted` with its events; with it, every event, call,
    /// step and agent of the session goes too (blob refcounts are
    /// released; the files are retention's), and the row itself.
    pub fn delete_session(&self, id: &SessionId, purge_traces: bool) -> Result<DeleteReport> {
        let ev = self.prepare(
            NewEvent::new(id.clone(), kinds::SESSION_STATUS)
                .payload(serde_json::json!({ "status": SessionStatus::Deleted })),
        )?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut report = DeleteReport::default();
        tx.execute("DELETE FROM session_fts WHERE session_id = ?1", params![id])?;
        report.messages = tx.execute(
            "DELETE FROM session_messages WHERE session_id = ?1",
            params![id],
        )? as u64;
        if !purge_traces {
            let n = tx.execute(
                "UPDATE sessions SET status = 'deleted', message_count = 0, updated_at = ?2 WHERE id = ?1",
                params![id, now_ts()],
            )?;
            require_row(n, "session", id.as_str())?;
            insert_event(&tx, ev)?;
            tx.commit()?;
            return Ok(report);
        }
        release_blobs(&tx, id)?;
        tx.execute(
            "DELETE FROM mentor_calls WHERE session_id = ?1",
            params![id],
        )?;
        report.events = tx.execute("DELETE FROM events WHERE session_id = ?1", params![id])? as u64;
        tx.execute(
            "DELETE FROM steps WHERE agent_id IN (SELECT id FROM agents WHERE session_id = ?1)",
            params![id],
        )?;
        tx.execute("DELETE FROM agents WHERE session_id = ?1", params![id])?;
        let n = tx.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        require_row(n, "session", id.as_str())?;
        tx.commit()?;
        Ok(report)
    }

    // ------------------------------------------------------------- export

    /// The session as one document: info, every message, every call.
    pub fn export_session(&self, id: &SessionId) -> Result<SessionExport> {
        let session = self.session_info(id)?;
        let messages = self
            .session_messages(id, 0, None)?
            .into_iter()
            .map(StoredMessage::into_wire)
            .collect::<Result<Vec<_>>>()?;
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!(
            "{MENTOR_CALL_COLUMNS} WHERE c.session_id = ?1 ORDER BY started_at, id"
        ))?;
        let calls = stmt
            .query_map(params![id], mentor_call_row)?
            .map(|r| {
                r.map(|c| MentorCallInfo {
                    id: c.id.into_string(),
                    agent_id: c.agent_id.into_string(),
                    step_id: c.step_id.into_string(),
                    kind: c.kind.as_str().to_owned(),
                    model: c.model,
                    effort: c.effort,
                    started_at: c.started_at,
                    ended_at: c.ended_at,
                    status: c.status.as_str().to_owned(),
                    stop_reason: c.stop_reason,
                    usage: c.usage,
                    cost_usd: c.cost_micros.map(micros_to_usd),
                    first_byte_ms: c.first_byte_ms,
                    total_ms: c.total_ms,
                })
            })
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(SessionExport {
            format: SESSION_EXPORT_FORMAT.to_owned(),
            exported_at: now_ts(),
            session,
            messages,
            mentor_calls: calls,
        })
    }

    /// Recreates an exported session under its own id: the row (its
    /// workspace link dropped, the path kept), the messages, and the
    /// calls behind one `mentor.request` event each (the bodies are
    /// not in an export). `conflict` when the id exists.
    pub fn import_session(&self, export: &SessionExport) -> Result<SessionId> {
        if export.format != SESSION_EXPORT_FORMAT {
            return Err(TraceError::invalid(
                "session export",
                format!(
                    "format `{}` (this build reads `{SESSION_EXPORT_FORMAT}`)",
                    export.format
                ),
            ));
        }
        let info = &export.session;
        let s = &info.summary;
        let id = SessionId::from(s.id.as_str());
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT COUNT(*) > 0 FROM sessions WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        if exists {
            return Err(TraceError::Conflict(format!("session {id} exists")));
        }
        let status = SessionStatus::parse(&s.status)
            .ok_or_else(|| TraceError::invalid("session status", s.status.clone()))?;
        tx.execute(
            "INSERT INTO sessions(id, created_at, updated_at, title, title_source, workspace_path,
                                  workspace_id, config_json, status, message_count,
                                  last_agent_status, prompt_version, tools_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, 0, ?9, ?10, ?11)",
            params![
                id,
                s.created_at,
                s.updated_at,
                s.title,
                info.title_source,
                s.workspace,
                info.config.to_string(),
                status.as_str(),
                s.last_agent_status.map(|st| agent_run_status(st).as_str()),
                info.prompt_version,
                info.tools_hash,
            ],
        )?;
        for m in &export.messages {
            let role = parse_role(&m.role)
                .ok_or_else(|| TraceError::invalid("message role", m.role.clone()))?;
            let content: Vec<ContentBlock> = serde_json::from_value(m.content.clone())?;
            put_message_at(
                &tx,
                &NewMessage {
                    session: id.clone(),
                    seq: m.seq,
                    role,
                    content,
                    agent: m.agent_id.as_deref().map(AgentId::from),
                    step: m.step_id.as_deref().map(StepId::from),
                },
                &m.created_at,
            )?;
        }
        for c in &export.mentor_calls {
            import_call(&tx, &id, c)?;
        }
        refresh_count(&tx, &id)?;
        tx.commit()?;
        Ok(id)
    }
}

// ------------------------------------------------------------------ helpers

/// The provisional title: the first line of the prompt, cut to
/// [`PROMPT_TITLE_CHARS`] characters with an ellipsis.
pub fn first_prompt_title(prompt: &str) -> String {
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut chars = line.chars();
    let head: String = chars.by_ref().take(PROMPT_TITLE_CHARS).collect();
    if chars.next().is_some() {
        format!("{}…", head.trim_end())
    } else {
        head
    }
}

/// An FTS5 expression for a user's words: each word quoted (so the
/// query syntax cannot be hit) as a prefix, all required. Empty when
/// there is no word to match.
pub fn fts_query(words: &str) -> String {
    words
        .split_whitespace()
        .map(|w| w.replace('"', ""))
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
    }
}

fn parse_role(s: &str) -> Option<Role> {
    Some(match s {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "system" => Role::System,
        _ => return None,
    })
}

fn agent_run_status(s: AgentStatus) -> RunStatus {
    match s {
        AgentStatus::Ok => RunStatus::Ok,
        AgentStatus::Cancelled => RunStatus::Cancelled,
        _ => RunStatus::Error,
    }
}

/// The text blocks of a message, one per line, for the search index.
fn search_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(ContentBlock::as_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn put_message(tx: &Connection, m: &NewMessage) -> Result<()> {
    put_message_at(tx, m, &now_ts())?;
    refresh_count(tx, &m.session)
}

pub(super) fn put_message_at(tx: &Connection, m: &NewMessage, created_at: &str) -> Result<()> {
    let content = serde_json::to_string(&m.content)?;
    tx.prepare_cached(
        "INSERT INTO session_messages(session_id, seq, role, content_json, agent_id, step_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(session_id, seq) DO UPDATE SET
             role = excluded.role, content_json = excluded.content_json,
             agent_id = excluded.agent_id, step_id = excluded.step_id",
    )?
    .execute(params![
        m.session,
        to_i64(m.seq),
        role_str(m.role),
        content,
        m.agent,
        m.step,
        created_at,
    ])?;
    tx.prepare_cached("DELETE FROM session_fts WHERE session_id = ?1 AND seq = ?2")?
        .execute(params![m.session, to_i64(m.seq)])?;
    let text = search_text(&m.content);
    if !text.is_empty() {
        tx.prepare_cached("INSERT INTO session_fts(session_id, seq, text) VALUES (?1, ?2, ?3)")?
            .execute(params![m.session, to_i64(m.seq), text])?;
    }
    Ok(())
}

pub(super) fn refresh_count(tx: &Connection, session: &SessionId) -> Result<()> {
    let n = tx
        .prepare_cached(
            "UPDATE sessions SET
                 message_count = (SELECT COUNT(*) FROM session_messages WHERE session_id = ?1),
                 updated_at = ?2
             WHERE id = ?1",
        )?
        .execute(params![session, now_ts()])?;
    require_row(n, "session", session.as_str())
}

/// Gives back the blob references of every event of `session` before
/// the events go.
fn release_blobs(tx: &Connection, session: &SessionId) -> Result<()> {
    let mut stmt = tx.prepare("SELECT payload_json, blob_id FROM events WHERE session_id = ?1")?;
    let rows = stmt.query_map(params![session], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for row in rows {
        let (payload, blob) = row?;
        if let Some(b) = blob {
            *counts.entry(b).or_default() += 1;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&payload) {
            for id in blob_refs(&v) {
                *counts.entry(id.into_string()).or_default() += 1;
            }
        }
    }
    for (id, n) in counts {
        tx.execute(
            "UPDATE blobs SET refcount = MAX(refcount - ?2, 0) WHERE id = ?1",
            params![id, n],
        )?;
    }
    Ok(())
}

/// A call from an export: one `mentor.request` event carries what the
/// export knows, and the row points at it.
fn import_call(tx: &Connection, session: &SessionId, c: &MentorCallInfo) -> Result<()> {
    let status = RunStatus::parse(&c.status)
        .ok_or_else(|| TraceError::invalid("call status", c.status.clone()))?;
    let kind = CallKind::parse(&c.kind).unwrap_or_default();
    let event_id = EventId::generate();
    tx.prepare_cached(
        "INSERT INTO events(id, session_id, agent_id, step_id, seq, ts, kind, payload_json)
         VALUES (?1, ?2, ?3, ?4,
                 (SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE session_id = ?2),
                 ?5, ?6, ?7)",
    )?
    .execute(params![
        event_id,
        session,
        c.agent_id,
        c.step_id,
        c.started_at,
        kinds::MENTOR_REQUEST,
        serde_json::json!({
            "call_id": c.id,
            "model": c.model,
            "effort": c.effort,
            "imported": true,
        })
        .to_string(),
    ])?;
    insert_mentor_call(
        tx,
        &MentorCallStart {
            id: c.id.as_str().into(),
            session: session.clone(),
            agent: c.agent_id.as_str().into(),
            step: c.step_id.as_str().into(),
            request_event: event_id,
            model: c.model.clone(),
            effort: c.effort.clone(),
            request_bytes: None,
            started_at: Some(c.started_at.clone()),
            kind,
        },
    )?;
    tx.prepare_cached(
        "UPDATE mentor_calls SET status = ?2, ended_at = ?3, stop_reason = ?4,
                input_tokens = ?5, output_tokens = ?6, cache_read_tokens = ?7, cache_creation_tokens = ?8,
                cost_micros = ?9, first_byte_ms = ?10, total_ms = ?11
         WHERE id = ?1",
    )?
    .execute(params![
        c.id,
        status.as_str(),
        c.ended_at,
        c.stop_reason,
        c.usage.map(|u| to_i64(u.input_tokens)),
        c.usage.map(|u| to_i64(u.output_tokens)),
        c.usage.map(|u| to_i64(u.cache_read_input_tokens)),
        c.usage.map(|u| to_i64(u.cache_creation_input_tokens)),
        c.cost_usd.map(usd_to_micros),
        c.first_byte_ms.map(to_i64),
        c.total_ms.map(to_i64),
    ])?;
    Ok(())
}

fn stored_message(r: &Row<'_>) -> rusqlite::Result<StoredMessage> {
    let role: String = r.get(1)?;
    let content: Vec<ContentBlock> =
        serde_json::from_str(&r.get::<_, String>(2)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
        })?;
    Ok(StoredMessage {
        seq: get_u64(r, 0)?,
        role: parse_role(&role).ok_or_else(|| bad_column("role", &role))?,
        content,
        agent_id: r.get(3)?,
        step_id: r.get(4)?,
        created_at: r.get(5)?,
    })
}

/// One row per session with its activity, token totals and cost.
const SUMMARY_COLUMNS: &str = "
    SELECT s.id, s.title, s.workspace_path, s.workspace_id, s.status, s.created_at, s.updated_at,
           s.message_count,
           COALESCE((SELECT MAX(created_at) FROM session_messages m WHERE m.session_id = s.id),
                    s.created_at) AS last_activity,
           s.last_agent_status,
           (SELECT COALESCE(SUM(input_tokens), 0) FROM mentor_calls c WHERE c.session_id = s.id),
           (SELECT COALESCE(SUM(output_tokens), 0) FROM mentor_calls c WHERE c.session_id = s.id),
           (SELECT COALESCE(SUM(cache_read_tokens), 0) FROM mentor_calls c WHERE c.session_id = s.id),
           (SELECT COALESCE(SUM(cache_creation_tokens), 0) FROM mentor_calls c WHERE c.session_id = s.id),
           (SELECT COALESCE(SUM(cost_micros), 0) FROM mentor_calls c WHERE c.session_id = s.id),
           (SELECT COUNT(*) FROM mentor_calls c
             WHERE c.session_id = s.id AND c.status = 'ok' AND c.cost_micros IS NULL),
           s.title_source, s.prompt_version, s.tools_hash, s.config_json,
           (SELECT COUNT(*) FROM mentor_calls c WHERE c.session_id = s.id)
    FROM sessions s";

fn session_info(r: &Row<'_>) -> rusqlite::Result<SessionInfo> {
    let last_status: Option<String> = r.get(9)?;
    let last_agent_status = last_status
        .map(|s| {
            RunStatus::parse(&s)
                .map(|st| match st {
                    RunStatus::Ok => AgentStatus::Ok,
                    RunStatus::Cancelled => AgentStatus::Cancelled,
                    RunStatus::Error | RunStatus::Running => AgentStatus::Error,
                })
                .ok_or_else(|| bad_column("last_agent_status", &s))
        })
        .transpose()?;
    let cost_micros: i64 = r.get(14)?;
    let unpriced: i64 = r.get(15)?;
    let title_source: Option<String> = r.get(16)?;
    if let Some(s) = &title_source
        && TitleSource::parse(s).is_none()
    {
        return Err(bad_column("title_source", s));
    }
    Ok(SessionInfo {
        summary: SessionSummary {
            id: r.get(0)?,
            title: r.get(1)?,
            workspace: r.get(2)?,
            workspace_id: r.get(3)?,
            status: r.get(4)?,
            created_at: r.get(5)?,
            updated_at: r.get(6)?,
            message_count: get_u64(r, 7)?,
            last_activity: r.get(8)?,
            last_agent_status,
            running_agent: None,
            usage: Usage {
                input_tokens: get_u64(r, 10)?,
                output_tokens: get_u64(r, 11)?,
                cache_read_input_tokens: get_u64(r, 12)?,
                cache_creation_input_tokens: get_u64(r, 13)?,
            },
            cost_usd: (unpriced == 0).then(|| micros_to_usd(cost_micros)),
            calls: get_u64(r, 20)?,
        },
        title_source,
        prompt_version: r.get(17)?,
        tools_hash: r.get(18)?,
        config: parse_json(&r.get::<_, String>(19)?)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_titles_take_the_first_line_and_cut_long_ones() {
        assert_eq!(
            first_prompt_title("  fix the tests\n\nplease"),
            "fix the tests"
        );
        assert_eq!(first_prompt_title("\n\n"), "");
        let long = "a".repeat(70);
        let title = first_prompt_title(&long);
        assert_eq!(title.chars().count(), PROMPT_TITLE_CHARS + 1);
        assert!(title.ends_with('…'));
        let exact = "b".repeat(PROMPT_TITLE_CHARS);
        assert_eq!(first_prompt_title(&exact), exact);
        assert_eq!(first_prompt_title("héllo wörld"), "héllo wörld");
    }

    #[test]
    fn fts_queries_quote_every_word_as_a_prefix() {
        assert_eq!(fts_query("hello world"), "\"hello\"* \"world\"*");
        assert_eq!(fts_query("  a \"b\" \"  "), "\"a\"* \"b\"*");
        assert_eq!(fts_query("OR NOT"), "\"OR\"* \"NOT\"*");
        assert_eq!(fts_query("   "), "");
    }
}
