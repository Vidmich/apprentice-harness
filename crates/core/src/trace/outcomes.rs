//! Outcome queries (task M01-15): the `outcome` and `agent.finished`
//! events of a session's agents for `session.get`, the counts behind
//! `stats.outcomes`, and the recovery of agents a previous daemon left
//! `running`.

use std::collections::{BTreeMap, HashMap, HashSet};

use apprentice_api::jsonrpc::RpcError;
use apprentice_api::types::{OutcomeInfo, OutcomeStats, StatsRange};
use rusqlite::{TransactionBehavior, params};
use serde_json::{Value, json};

use super::error::TraceError;
use super::store::{NewEvent, TraceStore, insert_event, parse_json};
use super::{AgentId, RunStatus, SessionId, kinds, now_ts};
use crate::outcomes::{Outcome, describe, info, kind};

type Result<T> = std::result::Result<T, TraceError>;

/// What `session.get` adds to each agent: its outcomes and, for a run
/// that did not end `ok`, the error of its `agent.finished`.
#[derive(Debug, Default)]
pub struct AgentSignals {
    pub outcomes: Vec<OutcomeInfo>,
    pub error: Option<RpcError>,
}

/// Filter for [`TraceStore::outcome_stats`]: bounds on the agent start
/// (inclusive `since`, exclusive `until`) and a workspace.
#[derive(Debug, Clone, Default)]
pub struct OutcomeFilter {
    pub since: Option<String>,
    pub until: Option<String>,
    pub workspace_id: Option<String>,
}

impl TraceStore {
    /// The signals of every agent of `session`, keyed by agent id.
    pub fn session_signals(&self, session: &SessionId) -> Result<HashMap<String, AgentSignals>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT id, agent_id, ts, kind, payload_json FROM events
             WHERE session_id = ?1 AND agent_id IS NOT NULL AND kind IN (?2, ?3)
             ORDER BY seq",
        )?;
        let rows = stmt.query_map(
            params![session, kinds::OUTCOME, kinds::AGENT_FINISHED],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    parse_json(&r.get::<_, String>(4)?)?,
                ))
            },
        )?;
        let mut out: HashMap<String, AgentSignals> = HashMap::new();
        for row in rows {
            let (id, agent, ts, kind, payload) = row?;
            let signals = out.entry(agent).or_default();
            if kind == kinds::OUTCOME {
                signals.outcomes.push(info(&id, &payload, &ts));
            } else if let Some(err) = payload.get("error")
                && !err.is_null()
            {
                signals.error = serde_json::from_value(err.clone()).ok();
            }
        }
        Ok(out)
    }

    /// The outcome events of one agent, oldest first.
    pub fn agent_outcomes(&self, agent: &AgentId) -> Result<Vec<OutcomeInfo>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT id, ts, payload_json FROM events
             WHERE agent_id = ?1 AND kind = ?2 ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![agent, kinds::OUTCOME], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                parse_json(&r.get::<_, String>(2)?)?,
            ))
        })?;
        rows.map(|row| {
            let (id, ts, payload) = row?;
            Ok(info(&id, &payload, &ts))
        })
        .collect()
    }

    /// The counts of `stats.outcomes` over the main agents started in
    /// the range.
    pub fn outcome_stats(&self, f: &OutcomeFilter) -> Result<OutcomeStats> {
        let conn = self.lock();
        let since = f.since.clone().unwrap_or_default();
        let until = f.until.clone().unwrap_or_default();
        let workspace = f.workspace_id.clone().unwrap_or_default();
        let filter = "a.kind = 'main'
             AND (?1 = '' OR a.created_at >= ?1)
             AND (?2 = '' OR a.created_at < ?2)
             AND (?3 = '' OR s.workspace_id = ?3)";
        let agents: u64 = conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM agents a JOIN sessions s ON s.id = a.session_id WHERE {filter}"
            ),
            params![since, until, workspace],
            |r| r.get::<_, i64>(0).map(|n| u64::try_from(n).unwrap_or(0)),
        )?;
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT e.agent_id, e.payload_json FROM events e
             JOIN agents a ON a.id = e.agent_id
             JOIN sessions s ON s.id = a.session_id
             WHERE e.kind = 'outcome' AND {filter}
             ORDER BY e.agent_id, e.seq"
        ))?;
        let rows = stmt.query_map(params![since, until, workspace], |r| {
            Ok((r.get::<_, String>(0)?, parse_json(&r.get::<_, String>(1)?)?))
        })?;
        let mut by_kind: BTreeMap<String, HashSet<String>> = BTreeMap::new();
        let mut last_tests: HashMap<String, bool> = HashMap::new();
        let mut errors: BTreeMap<String, HashSet<String>> = BTreeMap::new();
        for row in rows {
            let (agent, payload) = row?;
            let Some(k) = payload["kind"].as_str() else {
                continue;
            };
            let details = payload.get("details").cloned().unwrap_or(Value::Null);
            by_kind
                .entry(k.to_owned())
                .or_default()
                .insert(agent.clone());
            match k {
                kind::TESTS => {
                    let (_, ok) = describe(k, &details);
                    last_tests.insert(agent, ok.unwrap_or(false));
                }
                kind::ERROR => {
                    let what = details["kind"].as_str().unwrap_or("error").to_owned();
                    errors.entry(what).or_default().insert(agent);
                }
                _ => {}
            }
        }
        let count = |k: &str| by_kind.get(k).map_or(0, |s| s.len() as u64);
        let labelled: HashSet<&String> = kind::LABELS
            .iter()
            .filter_map(|k| by_kind.get(*k))
            .flatten()
            .collect();
        let labelled = labelled.len() as u64;
        #[allow(clippy::cast_precision_loss)] // counts, not money
        let labelled_share = if agents == 0 {
            0.0
        } else {
            (labelled as f64 / agents as f64 * 1000.0).round() / 1000.0
        };
        Ok(OutcomeStats {
            range: StatsRange {
                since: f.since.clone(),
                until: f.until.clone(),
            },
            agents,
            labelled,
            labelled_share,
            by_kind: by_kind
                .iter()
                .map(|(k, s)| (k.clone(), s.len() as u64))
                .collect(),
            tests_passed: last_tests.values().filter(|ok| **ok).count() as u64,
            tests_failed: last_tests.values().filter(|ok| !**ok).count() as u64,
            accepted: count(kind::USER_ACCEPT),
            rejected: count(kind::USER_REJECT),
            done: count(kind::TASK_DONE),
            errors: errors
                .into_iter()
                .map(|(k, s)| (k, s.len() as u64))
                .collect(),
        })
    }

    /// Ends every agent still `running` — none can be, this daemon
    /// having just opened the store: they belong to a daemon that died.
    /// Each gets `agent.finished {status: error, error: {kind:
    /// daemon_restart}}` and `outcome {kind: error, details: {kind:
    /// daemon_restart}}`; its open steps and mentor calls end the same
    /// way. Returns the agents recovered.
    pub fn recover_orphaned_agents(&self) -> Result<Vec<(AgentId, SessionId)>> {
        let orphans: Vec<(AgentId, SessionId)> = {
            let conn = self.lock();
            let mut stmt = conn.prepare(
                "SELECT id, session_id FROM agents WHERE status = 'running' ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for (agent, session) in &orphans {
            let error = RpcError::agent_stopped(
                "daemon_restart",
                "the daemon stopped while the agent was running; the run has no result",
            );
            let finished = self.prepare(
                NewEvent::new(session.clone(), kinds::AGENT_FINISHED)
                    .agent(agent.clone())
                    .payload(json!({
                        "status": RunStatus::Error,
                        "error": serde_json::to_value(&error)?,
                    })),
            )?;
            let outcome = self.prepare(
                Outcome::error(
                    "daemon_restart",
                    &json!({ "message": "the daemon stopped while the agent was running" }),
                )
                .event(session.clone(), agent.clone()),
            )?;
            let mut conn = self.lock();
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = now_ts();
            tx.execute(
                "UPDATE steps SET status = 'error', ended_at = ?2
                 WHERE agent_id = ?1 AND status = 'running'",
                params![agent, now],
            )?;
            tx.execute(
                "UPDATE mentor_calls SET status = 'error', ended_at = ?2
                 WHERE agent_id = ?1 AND status = 'running'",
                params![agent, now],
            )?;
            tx.execute(
                "UPDATE agents SET status = 'error', ended_at = ?2 WHERE id = ?1",
                params![agent, now],
            )?;
            tx.execute(
                "UPDATE sessions SET last_agent_status = 'error', updated_at = ?2 WHERE id = ?1",
                params![session, now],
            )?;
            insert_event(&tx, finished)?;
            insert_event(&tx, outcome)?;
            tx.commit()?;
        }
        Ok(orphans)
    }
}
