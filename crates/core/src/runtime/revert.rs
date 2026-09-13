//! The `reverted` outcome (task M01-15): at a run's start, whether the
//! previous run's changes to the workspace have been undone — the
//! quietest "no" a user gives. Best effort: a file the previous run
//! added is reverted when it is gone; one it deleted when it is back;
//! one it changed when the previous end snapshot's `git diff HEAD`
//! named it and the new start snapshot's does not, `HEAD` being the
//! same (a commit in between means the changes were kept, not
//! undone). Without git, changed files cannot be told apart from
//! further edits and are left alone.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use crate::app::AppState;
use crate::outcomes::{Outcome, kind};
use crate::trace::{AgentId, EventQuery, SessionId, TraceError, kinds};
use crate::workspace::{Snapshot, diff_paths};

/// The previous run of `session` (before `this`) whose `files_changed`
/// outcome the start snapshot `start` contradicts, and the outcome to
/// record for it.
///
/// # Errors
/// Store failure.
pub(super) fn detect(
    state: &Arc<AppState>,
    session: &SessionId,
    this: &AgentId,
    root: &Path,
    start: &Snapshot,
) -> Result<Option<(AgentId, Outcome)>, TraceError> {
    let store = state.store();
    let previous = store
        .list_agents(session)?
        .into_iter()
        .rev()
        .find(|a| &a.id != this);
    let Some(previous) = previous else {
        return Ok(None);
    };
    let changed = store
        .agent_outcomes(&previous.id)?
        .into_iter()
        .rev()
        .find(|o| o.kind == kind::FILES_CHANGED)
        .map(|o| o.details);
    let Some(changed) = changed else {
        return Ok(None);
    };
    let list = |key: &str| -> Vec<String> {
        changed[key]
            .as_array()
            .map(|v| {
                v.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let (added, deleted, modified) = (list("added"), list("deleted"), list("changed"));
    if added.is_empty() && deleted.is_empty() && modified.is_empty() {
        return Ok(None);
    }

    let mut reverted: Vec<String> = Vec::new();
    for p in &added {
        if !root.join(p).exists() && !start.contains(p) {
            reverted.push(p.clone());
        }
    }
    for p in &deleted {
        if root.join(p).exists() || start.contains(p) {
            reverted.push(p.clone());
        }
    }
    if !modified.is_empty()
        && let Some(end) = previous_end_snapshot(state, session, &previous.id)?
        && let Some(git) = &start.git
        && end.head.is_some()
        && end.head == git.head
    {
        let now_dirty: HashSet<String> = start
            .diff_paths()
            .into_iter()
            .chain(start.untracked().into_iter().map(str::to_owned))
            .collect();
        for p in &modified {
            if end.dirty_paths.contains(p) && !now_dirty.contains(p) {
                reverted.push(p.clone());
            }
        }
    }
    if reverted.is_empty() {
        return Ok(None);
    }
    reverted.sort_unstable();
    reverted.dedup();
    Ok(Some((previous.id, Outcome::reverted(&reverted, this))))
}

/// What the previous run's end snapshot said about git.
struct EndSnapshot {
    head: Option<String>,
    /// Paths in its `git diff HEAD` and its untracked list.
    dirty_paths: HashSet<String>,
}

fn previous_end_snapshot(
    state: &Arc<AppState>,
    session: &SessionId,
    agent: &AgentId,
) -> Result<Option<EndSnapshot>, TraceError> {
    let store = state.store();
    let events = store.list_events(&EventQuery {
        session_id: Some(session.clone()),
        agent_id: Some(agent.clone()),
        kinds: vec![kinds::WORKSPACE_SNAPSHOT.to_owned()],
        ..EventQuery::default()
    })?;
    for summary in events.iter().rev() {
        let ev = store.get_event(&summary.id.as_str().into())?;
        if ev.payload["phase"] != "end" {
            continue;
        }
        let mut dirty_paths: HashSet<String> = ev.payload["untracked"]
            .as_array()
            .map(|v| {
                v.iter()
                    .filter_map(|u| u["path"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(id) = &ev.blob_id {
            match store.read_blob(&id.as_str().into()) {
                Ok(diff) => dirty_paths.extend(diff_paths(&diff)),
                Err(e) => tracing::debug!(error = %e, "end snapshot diff unreadable"),
            }
        }
        return Ok(Some(EndSnapshot {
            head: ev.payload["git_head"].as_str().map(str::to_owned),
            dirty_paths,
        }));
    }
    Ok(None)
}
