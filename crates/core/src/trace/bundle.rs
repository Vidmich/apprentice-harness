//! The store side of trace bundles (task M01-14): which sessions a
//! selection names, their rows as [`Rows`], and the write of a bundle's
//! rows into this store. Packaging, redaction and the checks live in
//! [`crate::bundle`].

use std::collections::{HashMap, HashSet};
use std::path::Path;

use apprentice_api::types::{BundleSelection, ImportedSession};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;

use super::payload::blob_refs;
use super::sessions::{NewMessage, put_message_at, refresh_count};
use super::store::{TraceStore, get_u64, parse_json, to_i64};
use super::{AgentId, SessionId, StepId, TraceError, WorkspaceId, now_ts};
use crate::bundle::BundleError;
use crate::bundle::format::{
    AgentRow, BlobRow, CallRow, EventRow, MessageRow, Rows, SessionRow, StepRow, WorkspaceRow,
    read_blob,
};
use crate::mentor::{ContentBlock, Role};

type Result<T> = std::result::Result<T, BundleError>;

/// How [`TraceStore::import_bundle_rows`] writes.
#[derive(Debug, Clone, Default)]
pub struct ImportWrite {
    /// Attach every session to this workspace (must exist). `None`
    /// keeps a session's own workspace when its root is registered
    /// here or exists on disk (then it is registered), and leaves it
    /// unattached with its `workspace_path` otherwise.
    pub into_workspace: Option<String>,
    /// Keep the bundle's ids; an existing one is a conflict. Otherwise a
    /// session whose id exists gets fresh ids for itself and everything
    /// under it.
    pub keep_ids: bool,
    /// Recorded under `import` in each session's `config_json`.
    pub provenance: Value,
}

/// What an import wrote.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportOutcome {
    pub sessions: Vec<ImportedSession>,
    /// Blob files that were new to the store.
    pub blobs_written: u64,
}

impl TraceStore {
    /// The sessions `sel` names, oldest first. Every explicit id must
    /// exist; with nothing selecting and `all` unset, an error.
    pub fn select_bundle_sessions(&self, sel: &BundleSelection) -> Result<Vec<SessionId>> {
        let mut clauses: Vec<String> = Vec::new();
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        let mut bind = |clause: &str, v: rusqlite::types::Value| {
            args.push(v);
            clauses.push(clause.replace('?', &format!("?{}", args.len())));
        };
        if let Some(ws) = &sel.workspace_id {
            bind("workspace_id = ?", ws.clone().into());
        }
        if let Some(since) = &sel.since {
            bind("created_at >= ?", since.clone().into());
        }
        if let Some(until) = &sel.until {
            bind("created_at < ?", until.clone().into());
        }
        let conn = self.lock();
        if !sel.session_ids.is_empty() {
            for id in &sel.session_ids {
                let exists: bool = conn.query_row(
                    "SELECT COUNT(*) > 0 FROM sessions WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )?;
                if !exists {
                    return Err(BundleError::NotFound {
                        what: "session",
                        id: id.clone(),
                    });
                }
            }
            let marks = (0..sel.session_ids.len())
                .map(|i| format!("?{}", args.len() + i + 1))
                .collect::<Vec<_>>()
                .join(", ");
            clauses.push(format!("id IN ({marks})"));
            args.extend(sel.session_ids.iter().cloned().map(Into::into));
        } else if clauses.is_empty() && !sel.all {
            return Err(BundleError::Invalid(
                "nothing selects a session: pass session ids, a workspace, a time range, or all"
                    .to_owned(),
            ));
        }
        let where_sql = if clauses.is_empty() {
            "1".to_owned()
        } else {
            clauses.join(" AND ")
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT id FROM sessions WHERE {where_sql} ORDER BY created_at, id"
        ))?;
        let ids = stmt
            .query_map(rusqlite::params_from_iter(args), |r| {
                r.get::<_, SessionId>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    /// Every row of `sessions` (in the given order) and of the workspaces
    /// and blobs they reference. A referenced blob without a row is left
    /// out (an `integrity_check` finding, not the bundle's).
    pub fn bundle_rows(&self, sessions: &[SessionId]) -> Result<Rows> {
        let conn = self.lock();
        let mut rows = Rows::default();
        let mut workspace_ids: Vec<String> = Vec::new();
        let mut blob_ids: Vec<String> = Vec::new();
        let mut seen_blobs: HashSet<String> = HashSet::new();
        for id in sessions {
            let session = conn
                .query_row(
                    "SELECT id, created_at, updated_at, title, title_source, workspace_path,
                            workspace_id, config_json, status, message_count, last_agent_status,
                            prompt_version, tools_hash
                     FROM sessions WHERE id = ?1",
                    params![id],
                    |r| {
                        Ok(SessionRow {
                            id: r.get(0)?,
                            created_at: r.get(1)?,
                            updated_at: r.get(2)?,
                            title: r.get(3)?,
                            title_source: r.get(4)?,
                            workspace_path: r.get(5)?,
                            workspace_id: r.get(6)?,
                            config: parse_json(&r.get::<_, String>(7)?)?,
                            status: r.get(8)?,
                            message_count: get_u64(r, 9)?,
                            last_agent_status: r.get(10)?,
                            prompt_version: r.get(11)?,
                            tools_hash: r.get(12)?,
                        })
                    },
                )
                .optional()?
                .ok_or_else(|| BundleError::NotFound {
                    what: "session",
                    id: id.to_string(),
                })?;
            if let Some(ws) = &session.workspace_id
                && !workspace_ids.contains(ws)
            {
                workspace_ids.push(ws.clone());
            }
            rows.sessions.push(session);

            let mut stmt = conn.prepare_cached(
                "SELECT id, session_id, parent_agent_id, kind, created_at, ended_at, status,
                        task_text, options_json
                 FROM agents WHERE session_id = ?1 ORDER BY created_at, id",
            )?;
            let agents = stmt
                .query_map(params![id], |r| {
                    Ok(AgentRow {
                        id: r.get(0)?,
                        session_id: r.get(1)?,
                        parent_agent_id: r.get(2)?,
                        kind: r.get(3)?,
                        created_at: r.get(4)?,
                        ended_at: r.get(5)?,
                        status: r.get(6)?,
                        task_text: r.get(7)?,
                        options: parse_json(&r.get::<_, String>(8)?)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut stmt = conn.prepare_cached(
                "SELECT id, agent_id, seq, started_at, ended_at, status FROM steps
                 WHERE agent_id = ?1 ORDER BY seq",
            )?;
            for agent in &agents {
                let steps = stmt
                    .query_map(params![agent.id], |r| {
                        Ok(StepRow {
                            id: r.get(0)?,
                            agent_id: r.get(1)?,
                            seq: get_u64(r, 2)?,
                            started_at: r.get(3)?,
                            ended_at: r.get(4)?,
                            status: r.get(5)?,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows.steps.extend(steps);
            }
            rows.agents.extend(agents);

            let mut stmt = conn.prepare_cached(
                "SELECT id, session_id, agent_id, step_id, seq, ts, kind, payload_json, blob_id
                 FROM events WHERE session_id = ?1 ORDER BY seq",
            )?;
            let events = stmt
                .query_map(params![id], |r| {
                    Ok(EventRow {
                        id: r.get(0)?,
                        session_id: r.get(1)?,
                        agent_id: r.get(2)?,
                        step_id: r.get(3)?,
                        seq: get_u64(r, 4)?,
                        ts: r.get(5)?,
                        kind: r.get(6)?,
                        payload: parse_json(&r.get::<_, String>(7)?)?,
                        blob_id: r.get(8)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for ev in &events {
                for b in ev.blob_id.iter().cloned().chain(
                    blob_refs(&ev.payload)
                        .into_iter()
                        .map(super::BlobId::into_string),
                ) {
                    if seen_blobs.insert(b.clone()) {
                        blob_ids.push(b);
                    }
                }
            }
            rows.events.extend(events);

            let mut stmt = conn.prepare_cached(
                "SELECT id, session_id, agent_id, step_id, request_event_id, response_event_id,
                        model, effort, started_at, ended_at, status, stop_reason, http_status,
                        input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                        cost_micros, first_byte_ms, total_ms, request_bytes, apprentice_applied,
                        kind
                 FROM mentor_calls WHERE session_id = ?1 ORDER BY started_at, id",
            )?;
            let calls = stmt
                .query_map(params![id], |r| {
                    Ok(CallRow {
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
                        status: r.get(10)?,
                        stop_reason: r.get(11)?,
                        http_status: r.get(12)?,
                        input_tokens: r.get(13)?,
                        output_tokens: r.get(14)?,
                        cache_read_tokens: r.get(15)?,
                        cache_creation_tokens: r.get(16)?,
                        cost_micros: r.get(17)?,
                        first_byte_ms: r.get(18)?,
                        total_ms: r.get(19)?,
                        request_bytes: r.get(20)?,
                        apprentice_applied: r.get::<_, i64>(21)? != 0,
                        kind: r.get(22)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.mentor_calls.extend(calls);

            let mut stmt = conn.prepare_cached(
                "SELECT session_id, seq, role, content_json, agent_id, step_id, created_at
                 FROM session_messages WHERE session_id = ?1 ORDER BY seq",
            )?;
            let messages = stmt
                .query_map(params![id], |r| {
                    Ok(MessageRow {
                        session_id: r.get(0)?,
                        seq: get_u64(r, 1)?,
                        role: r.get(2)?,
                        content: parse_json(&r.get::<_, String>(3)?)?,
                        agent_id: r.get(4)?,
                        step_id: r.get(5)?,
                        created_at: r.get(6)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.messages.extend(messages);
        }

        workspace_ids.sort();
        let mut stmt = conn.prepare_cached(
            "SELECT id, root, name, created_at, last_used_at, settings_json FROM workspaces WHERE id = ?1",
        )?;
        for ws in &workspace_ids {
            let row = stmt
                .query_row(params![ws], |r| {
                    Ok(WorkspaceRow {
                        id: r.get(0)?,
                        root: r.get(1)?,
                        name: r.get(2)?,
                        created_at: r.get(3)?,
                        last_used_at: r.get(4)?,
                        settings: parse_json(&r.get::<_, String>(5)?)?,
                    })
                })
                .optional()?;
            rows.workspaces.extend(row);
        }

        blob_ids.sort();
        let mut stmt = conn.prepare_cached(
            "SELECT id, size, media_type, created_at, pruned_at IS NOT NULL FROM blobs WHERE id = ?1",
        )?;
        for b in &blob_ids {
            let row = stmt
                .query_row(params![b], |r| {
                    Ok(BlobRow {
                        id: r.get(0)?,
                        size: get_u64(r, 1)?,
                        media_type: r.get(2)?,
                        created_at: r.get(3)?,
                        pruned: r.get(4)?,
                    })
                })
                .optional()?;
            rows.blobs.extend(row);
        }
        Ok(rows)
    }

    /// Writes `rows` (whose blobs are files under `dir`, already
    /// verified) into this store in one transaction; blob files go in
    /// first, content-addressed, so a failed transaction leaves nothing
    /// dangling but files the next import reuses.
    pub fn import_bundle_rows(
        &self,
        dir: &Path,
        rows: &Rows,
        write: &ImportWrite,
    ) -> Result<ImportOutcome> {
        let mut blobs_written = 0;
        for b in rows.blobs.iter().filter(|b| !b.pruned) {
            let bytes = read_blob(dir, &b.id)?;
            let (id, fresh) = self.files.write(&bytes)?;
            if id != b.id {
                return Err(BundleError::BlobCorrupted {
                    id: b.id.clone(),
                    actual: id,
                });
            }
            if fresh {
                blobs_written += 1;
            }
        }

        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(ws) = &write.into_workspace {
            let exists: bool = tx.query_row(
                "SELECT COUNT(*) > 0 FROM workspaces WHERE id = ?1",
                params![ws],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(BundleError::NotFound {
                    what: "workspace",
                    id: ws.clone(),
                });
            }
        }

        // Ids: kept, or fresh for every session whose id is taken.
        let mut map: HashMap<String, String> = HashMap::new();
        let mut imported = Vec::new();
        for s in &rows.sessions {
            let taken = id_taken(&tx, "sessions", &s.id)?;
            if taken && write.keep_ids {
                return Err(BundleError::Conflict(format!("session {} exists", s.id)));
            }
            let new_id = if taken {
                SessionId::generate().into_string()
            } else {
                s.id.clone()
            };
            imported.push(ImportedSession {
                from: s.id.clone(),
                to: new_id.clone(),
            });
            if taken {
                map.insert(s.id.clone(), new_id);
                for a in rows.agents.iter().filter(|a| a.session_id == s.id) {
                    map.insert(a.id.clone(), AgentId::generate().into_string());
                    for st in rows.steps.iter().filter(|st| st.agent_id == a.id) {
                        map.insert(st.id.clone(), StepId::generate().into_string());
                    }
                }
                for e in rows.events.iter().filter(|e| e.session_id == s.id) {
                    map.insert(e.id.clone(), super::EventId::generate().into_string());
                }
                for c in rows.mentor_calls.iter().filter(|c| c.session_id == s.id) {
                    map.insert(c.id.clone(), super::CallId::generate().into_string());
                }
            }
        }
        if write.keep_ids {
            for (table, ids) in [
                (
                    "agents",
                    rows.agents.iter().map(|a| &a.id).collect::<Vec<_>>(),
                ),
                ("steps", rows.steps.iter().map(|s| &s.id).collect()),
                ("events", rows.events.iter().map(|e| &e.id).collect()),
                (
                    "mentor_calls",
                    rows.mentor_calls.iter().map(|c| &c.id).collect(),
                ),
            ] {
                for id in ids {
                    if id_taken(&tx, table, id)? {
                        return Err(BundleError::Conflict(format!(
                            "{} {id} exists",
                            table.trim_end_matches('s').replace('_', " ")
                        )));
                    }
                }
            }
        }
        let id = |old: &str| -> String { map.get(old).cloned().unwrap_or_else(|| old.to_owned()) };
        let opt_id = |old: &Option<String>| -> Option<String> { old.as_deref().map(id) };

        // Workspaces: bundle id → local id, for the roots this machine has.
        let mut workspaces: HashMap<String, String> = HashMap::new();
        if write.into_workspace.is_none() {
            for w in &rows.workspaces {
                let existing: Option<String> = tx
                    .query_row(
                        "SELECT id FROM workspaces WHERE root = ?1",
                        params![w.root],
                        |r| r.get(0),
                    )
                    .optional()?;
                let local = match existing {
                    Some(id) => id,
                    None if Path::new(&w.root).is_dir() => {
                        let id = if id_taken(&tx, "workspaces", &w.id)? {
                            WorkspaceId::generate().into_string()
                        } else {
                            w.id.clone()
                        };
                        tx.execute(
                            "INSERT INTO workspaces(id, root, name, created_at, last_used_at, settings_json)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            params![
                                id,
                                w.root,
                                w.name,
                                w.created_at,
                                w.last_used_at,
                                w.settings.to_string()
                            ],
                        )?;
                        id
                    }
                    None => continue,
                };
                workspaces.insert(w.id.clone(), local);
            }
        }

        for s in &rows.sessions {
            let mut config = s.config.clone();
            if !config.is_object() {
                config = Value::Object(serde_json::Map::new());
            }
            let mut provenance = write.provenance.clone();
            if let Value::Object(p) = &mut provenance {
                p.insert(
                    "original_session_id".to_owned(),
                    Value::String(s.id.clone()),
                );
            }
            config["import"] = provenance;
            let workspace_id = write.into_workspace.clone().or_else(|| {
                s.workspace_id
                    .as_ref()
                    .and_then(|w| workspaces.get(w).cloned())
            });
            tx.execute(
                "INSERT INTO sessions(id, created_at, updated_at, title, title_source, workspace_path,
                                      workspace_id, config_json, status, message_count,
                                      last_agent_status, prompt_version, tools_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, ?11, ?12)",
                params![
                    id(&s.id),
                    s.created_at,
                    s.updated_at,
                    s.title,
                    s.title_source,
                    s.workspace_path,
                    workspace_id,
                    config.to_string(),
                    s.status,
                    s.last_agent_status,
                    s.prompt_version,
                    s.tools_hash,
                ],
            )?;
        }
        for a in &rows.agents {
            tx.execute(
                "INSERT INTO agents(id, session_id, parent_agent_id, kind, created_at, ended_at,
                                    status, task_text, options_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id(&a.id),
                    id(&a.session_id),
                    opt_id(&a.parent_agent_id),
                    a.kind,
                    a.created_at,
                    a.ended_at,
                    a.status,
                    a.task_text,
                    a.options.to_string(),
                ],
            )?;
        }
        for st in &rows.steps {
            tx.execute(
                "INSERT INTO steps(id, agent_id, seq, started_at, ended_at, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id(&st.id),
                    id(&st.agent_id),
                    to_i64(st.seq),
                    st.started_at,
                    st.ended_at,
                    st.status
                ],
            )?;
        }

        // Blob rows: one reference per event that names the blob.
        let mut refs: HashMap<String, i64> = HashMap::new();
        for e in &rows.events {
            if let Some(b) = &e.blob_id {
                *refs.entry(b.clone()).or_default() += 1;
            }
            for b in blob_refs(&e.payload) {
                *refs.entry(b.into_string()).or_default() += 1;
            }
        }
        for b in &rows.blobs {
            let n = refs.get(&b.id).copied().unwrap_or(0).max(1);
            tx.execute(
                "INSERT INTO blobs(id, size, media_type, created_at, refcount, pruned_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET refcount = refcount + ?5",
                params![
                    b.id,
                    to_i64(b.size),
                    b.media_type,
                    b.created_at,
                    n,
                    b.pruned.then(now_ts),
                ],
            )?;
        }

        for e in &rows.events {
            let mut payload = e.payload.clone();
            if !map.is_empty() {
                remap_strings(&mut payload, &map);
            }
            tx.execute(
                "INSERT INTO events(id, session_id, agent_id, step_id, seq, ts, kind, payload_json, blob_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id(&e.id),
                    id(&e.session_id),
                    opt_id(&e.agent_id),
                    opt_id(&e.step_id),
                    to_i64(e.seq),
                    e.ts,
                    e.kind,
                    payload.to_string(),
                    e.blob_id,
                ],
            )?;
        }
        for c in &rows.mentor_calls {
            tx.execute(
                "INSERT INTO mentor_calls(id, session_id, agent_id, step_id, request_event_id,
                        response_event_id, model, effort, started_at, ended_at, status, stop_reason,
                        http_status, input_tokens, output_tokens, cache_read_tokens,
                        cache_creation_tokens, cost_micros, first_byte_ms, total_ms, request_bytes,
                        apprentice_applied, kind)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                         ?18, ?19, ?20, ?21, ?22, ?23)",
                params![
                    id(&c.id),
                    id(&c.session_id),
                    id(&c.agent_id),
                    id(&c.step_id),
                    id(&c.request_event_id),
                    opt_id(&c.response_event_id),
                    c.model,
                    c.effort,
                    c.started_at,
                    c.ended_at,
                    c.status,
                    c.stop_reason,
                    c.http_status,
                    c.input_tokens,
                    c.output_tokens,
                    c.cache_read_tokens,
                    c.cache_creation_tokens,
                    c.cost_micros,
                    c.first_byte_ms,
                    c.total_ms,
                    c.request_bytes,
                    i64::from(c.apprentice_applied),
                    c.kind,
                ],
            )?;
        }
        for m in &rows.messages {
            let role = parse_role(&m.role)
                .ok_or_else(|| BundleError::Invalid(format!("message role `{}`", m.role)))?;
            let content: Vec<ContentBlock> = serde_json::from_value(m.content.clone())?;
            put_message_at(
                &tx,
                &NewMessage {
                    session: SessionId::from(id(&m.session_id)),
                    seq: m.seq,
                    role,
                    content,
                    agent: opt_id(&m.agent_id).map(AgentId::from),
                    step: opt_id(&m.step_id).map(StepId::from),
                },
                &m.created_at,
            )?;
        }
        for s in &imported {
            refresh_count(&tx, &SessionId::from(s.to.as_str()))?;
            // `refresh_count` stamps `updated_at`; keep the bundle's.
            if let Some(row) = rows.sessions.iter().find(|r| r.id == s.from) {
                tx.execute(
                    "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
                    params![s.to, row.updated_at],
                )?;
            }
        }
        tx.commit()?;
        Ok(ImportOutcome {
            sessions: imported,
            blobs_written,
        })
    }
}

fn id_taken(conn: &Connection, table: &str, id: &str) -> std::result::Result<bool, TraceError> {
    Ok(conn.query_row(
        &format!("SELECT COUNT(*) > 0 FROM {table} WHERE id = ?1"),
        params![id],
        |r| r.get(0),
    )?)
}

fn parse_role(s: &str) -> Option<Role> {
    Some(match s {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "system" => Role::System,
        _ => return None,
    })
}

/// Replaces every string equal to a remapped id (payloads name calls,
/// agents and events by id).
fn remap_strings(value: &mut Value, map: &HashMap<String, String>) {
    match value {
        Value::String(s) => {
            if let Some(new) = map.get(s.as_str()) {
                *s = new.clone();
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| remap_strings(v, map)),
        Value::Object(fields) => fields.values_mut().for_each(|v| remap_strings(v, map)),
        _ => {}
    }
}
