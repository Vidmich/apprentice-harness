//! `replay-check`: for every selected mentor call, the stored
//! `mentor.request` body is there, hashes to its id and to the payload's
//! `request_hash`, parses as the wire request and serialises back to the
//! same bytes. With `rebuild`, the request is built again from the
//! session's stored messages and the call's prefix and compared — the
//! property the replay evaluator (M04) relies on.

use std::collections::HashMap;

use apprentice_api::types::{ReplayCall, ReplayReport, ReplayStatus};
use serde_json::Value;

use super::BundleError;
use crate::mentor::{CacheFlag, Message, WireRequest, request_body};
use crate::runtime::{Conversation, MENTOR_SYSTEM_V1, PROMPT_VERSION, SystemPrompt};
use crate::trace::{
    BlobId, CallFilter, CallId, CallKind, EventQuery, MentorCallRow, SessionId, TraceError,
    TraceStore, kinds, sha256_hex,
};

/// Which calls to check; nothing set means every call in the store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplaySelection {
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub call_id: Option<String>,
    /// Bounds on the call start, resolved timestamps (task M01-15).
    pub since: Option<String>,
    pub until: Option<String>,
}

/// Runs the checks over the selection.
///
/// # Errors
/// An unknown call id, or a store failure.
pub fn replay_check(
    store: &TraceStore,
    sel: &ReplaySelection,
    rebuild: bool,
) -> Result<ReplayReport, BundleError> {
    let calls = match &sel.call_id {
        Some(id) => vec![store.get_mentor_call(&CallId::from(id.as_str()))?],
        None => store.list_mentor_calls(&CallFilter {
            session_id: sel.session_id.as_deref().map(SessionId::from),
            agent_id: sel.agent_id.as_deref().map(Into::into),
            since: sel.since.clone(),
            until: sel.until.clone(),
            ..CallFilter::default()
        })?,
    };
    let mut sessions: HashMap<SessionId, SessionContext> = HashMap::new();
    let mut report = ReplayReport {
        rebuild,
        ..ReplayReport::default()
    };
    for call in &calls {
        let context = if rebuild {
            if !sessions.contains_key(&call.session_id) {
                let c = SessionContext::load(store, &call.session_id)?;
                sessions.insert(call.session_id.clone(), c);
            }
            sessions.get(&call.session_id)
        } else {
            None
        };
        let checked = check_call(store, call, context)?;
        report.checked += 1;
        match checked.status {
            ReplayStatus::Ok => report.passed += 1,
            ReplayStatus::Failed => report.failed += 1,
            ReplayStatus::Skipped => report.skipped += 1,
        }
        report.calls.push(checked);
    }
    Ok(report)
}

/// What a rebuild needs of a session, loaded once.
struct SessionContext {
    messages: Vec<Message>,
    tools_hash: Option<String>,
    /// The tool set changed during the session, so older calls carry
    /// a hash the row no longer has.
    prefix_changed: bool,
}

impl SessionContext {
    fn load(store: &TraceStore, session: &SessionId) -> Result<Self, BundleError> {
        let record = store.get_session(session)?;
        let messages = store
            .session_messages(session, 0, None)?
            .into_iter()
            .map(|r| Message {
                role: r.role,
                content: r.content,
            })
            .collect();
        let prefix_changed = !store
            .list_events(&EventQuery {
                kinds: vec![kinds::SESSION_PREFIX_CHANGED.to_owned()],
                limit: Some(1),
                ..EventQuery::session(session.clone())
            })?
            .is_empty();
        Ok(Self {
            messages,
            tools_hash: record.tools_hash,
            prefix_changed,
        })
    }
}

fn check_call(
    store: &TraceStore,
    call: &MentorCallRow,
    context: Option<&SessionContext>,
) -> Result<ReplayCall, BundleError> {
    let mut out = ReplayCall {
        call_id: call.id.to_string(),
        session_id: call.session_id.to_string(),
        agent_id: call.agent_id.to_string(),
        step_id: call.step_id.to_string(),
        kind: call.kind.as_str().to_owned(),
        request_event_id: call.request_event_id.to_string(),
        status: ReplayStatus::Ok,
        checks: Vec::new(),
        problems: Vec::new(),
    };
    let fail = |out: &mut ReplayCall, check: &str, problem: String| {
        out.checks.push(check.to_owned());
        out.problems.push(problem);
        out.status = ReplayStatus::Failed;
    };

    // blob: the request event carries a body.
    let event = match store.get_event(&call.request_event_id) {
        Ok(e) => e,
        Err(TraceError::NotFound { .. }) => {
            fail(
                &mut out,
                "blob",
                format!("request event {} is gone", call.request_event_id),
            );
            return Ok(out);
        }
        Err(e) => return Err(e.into()),
    };
    if event.summary.kind != kinds::MENTOR_REQUEST {
        fail(
            &mut out,
            "blob",
            format!(
                "event {} is `{}`, not `mentor.request`",
                event.summary.id, event.summary.kind
            ),
        );
        return Ok(out);
    }
    let Some(blob_id) = &event.blob_id else {
        fail(
            &mut out,
            "blob",
            "the request event has no body blob (an import without bodies?)".to_owned(),
        );
        return Ok(out);
    };
    let bytes = match store.read_blob(&BlobId::from(blob_id.as_str())) {
        Ok(b) => b,
        Err(TraceError::NotFound { .. }) => {
            fail(
                &mut out,
                "blob",
                format!("body blob {blob_id} has no file (pruned?)"),
            );
            return Ok(out);
        }
        Err(e) => return Err(e.into()),
    };
    out.checks.push("blob".to_owned());

    // hash: the file is the blob, and the payload names it.
    let actual = sha256_hex(&bytes);
    if actual != *blob_id {
        fail(
            &mut out,
            "hash",
            format!("body hashes to {actual}, not its id {blob_id}"),
        );
        return Ok(out);
    }
    if let Some(Value::String(h)) = event.payload.get("request_hash")
        && h != &actual
    {
        fail(
            &mut out,
            "hash",
            format!("payload request_hash {h} names another body than {actual}"),
        );
        return Ok(out);
    }
    if let Some(n) = event.payload.get("bytes").and_then(Value::as_u64)
        && n != bytes.len() as u64
    {
        fail(
            &mut out,
            "hash",
            format!("payload says {n} bytes, the body has {}", bytes.len()),
        );
        return Ok(out);
    }
    out.checks.push("hash".to_owned());

    // parse + canonical: the body is our wire shape, and only that.
    let wire: WireRequest = match serde_json::from_slice(&bytes) {
        Ok(w) => w,
        Err(e) => {
            fail(
                &mut out,
                "parse",
                format!("body does not parse as a request: {e}"),
            );
            return Ok(out);
        }
    };
    out.checks.push("parse".to_owned());
    let again = serde_json::to_vec(&wire)?;
    if again != bytes {
        let at = first_difference(&bytes, &again);
        fail(
            &mut out,
            "canonical",
            format!(
                "re-serialising the parsed body differs at byte {at} of {} (something the wire types drop)",
                bytes.len()
            ),
        );
        return Ok(out);
    }
    out.checks.push("canonical".to_owned());

    let Some(context) = context else {
        return Ok(out);
    };
    if call.kind != CallKind::Step {
        out.checks.push("rebuild".to_owned());
        out.problems.push(format!(
            "rebuild skipped: a `{}` call is not built from the conversation",
            call.kind.as_str()
        ));
        out.status = ReplayStatus::Skipped;
        return Ok(out);
    }
    match rebuild(call, &event.payload, &wire, &bytes, context) {
        Ok(()) => out.checks.push("rebuild".to_owned()),
        Err(problem) => fail(&mut out, "rebuild", problem),
    }
    Ok(out)
}

/// Builds the request the runtime would have built at this call from
/// the stored conversation and compares it to the stored body.
fn rebuild(
    call: &MentorCallRow,
    payload: &Value,
    wire: &WireRequest,
    bytes: &[u8],
    context: &SessionContext,
) -> Result<(), String> {
    let n = payload
        .get("message_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| "payload has no message_count (recorded before M00-11?)".to_owned())?;
    let n = usize::try_from(n).unwrap_or(usize::MAX);
    if context.messages.len() < n {
        return Err(format!(
            "the session has {} stored messages, the call saw {n}",
            context.messages.len()
        ));
    }
    let prompt_version = payload
        .get("prompt_version")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if prompt_version == PROMPT_VERSION {
        match wire.system.first() {
            Some(core) if core.text == MENTOR_SYSTEM_V1 => {}
            Some(_) => {
                return Err(format!(
                    "the core system block is not `{PROMPT_VERSION}` as this build embeds it"
                ));
            }
            None => return Err("the body has no system prompt".to_owned()),
        }
    }
    let mut tools = wire.tools.clone();
    for t in &mut tools {
        t.cache = CacheFlag(false);
    }
    let mut conv = Conversation::load(call.session_id.clone(), context.messages[..n].to_vec());
    conv.set_system(SystemPrompt {
        version: prompt_version.to_owned(),
        blocks: wire
            .system
            .iter()
            .map(|b| crate::mentor::SystemBlock::new(b.text.clone()))
            .collect(),
    });
    conv.set_tools(tools);
    if let (Some(stored), Some(built)) = (&context.tools_hash, conv.tools_hash())
        && stored != built
        && !context.prefix_changed
    {
        return Err(format!(
            "the body's tool set hashes to {built}, the session recorded {stored}"
        ));
    }
    conv.model.clone_from(&call.model);
    conv.max_tokens = wire.max_tokens;
    conv.thinking = wire.thinking;
    conv.effort = wire.output_config.effort;
    let rebuilt = request_body(&conv.request()).map_err(|e| e.to_string())?;
    if rebuilt == bytes {
        return Ok(());
    }
    let stored: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let built: Value = serde_json::from_slice(&rebuilt).map_err(|e| e.to_string())?;
    Err(first_diff(&built, &stored, "$")
        .unwrap_or_else(|| "the rebuilt body differs (same JSON, different bytes)".to_owned()))
}

fn first_difference(a: &[u8], b: &[u8]) -> usize {
    a.iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()))
}

/// The first place `expected` (rebuilt) and `actual` (stored) differ.
fn first_diff(expected: &Value, actual: &Value, path: &str) -> Option<String> {
    match (expected, actual) {
        (Value::Object(e), Value::Object(a)) => {
            for (k, ev) in e {
                match a.get(k) {
                    Some(av) => {
                        if let Some(d) = first_diff(ev, av, &format!("{path}.{k}")) {
                            return Some(d);
                        }
                    }
                    None => return Some(format!("{path}.{k}: rebuilt has it, the body does not")),
                }
            }
            a.keys()
                .find(|k| !e.contains_key(*k))
                .map(|k| format!("{path}.{k}: the body has it, the rebuilt does not"))
        }
        (Value::Array(e), Value::Array(a)) => {
            for (i, (ev, av)) in e.iter().zip(a).enumerate() {
                if let Some(d) = first_diff(ev, av, &format!("{path}[{i}]")) {
                    return Some(d);
                }
            }
            (e.len() != a.len()).then(|| {
                format!(
                    "{path}: rebuilt has {} items, the body {}",
                    e.len(),
                    a.len()
                )
            })
        }
        _ if expected == actual => None,
        _ => Some(format!(
            "{path}: rebuilt {} vs body {}",
            snippet(expected),
            snippet(actual)
        )),
    }
}

/// A short rendering for a diff line.
fn snippet(v: &Value) -> String {
    let s = v.to_string();
    let mut chars = s.chars();
    let short: String = chars.by_ref().take(60).collect();
    if chars.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn diff_names_the_first_path() {
        let a = json!({"messages": [{"role": "user", "content": [{"text": "a"}]}], "n": 1});
        let b = json!({"messages": [{"role": "user", "content": [{"text": "b"}]}], "n": 1});
        assert_eq!(
            first_diff(&a, &b, "$").unwrap(),
            "$.messages[0].content[0].text: rebuilt \"a\" vs body \"b\""
        );
        assert_eq!(first_diff(&a, &a, "$"), None);
        let c = json!({"messages": [], "n": 1});
        assert_eq!(
            first_diff(&a, &c, "$").unwrap(),
            "$.messages: rebuilt has 1 items, the body 0"
        );
        let d = json!({"messages": a["messages"].clone()});
        assert_eq!(
            first_diff(&a, &d, "$").unwrap(),
            "$.n: rebuilt has it, the body does not"
        );
        assert_eq!(first_difference(b"abc", b"abd"), 2);
        assert_eq!(first_difference(b"abc", b"abcd"), 3);
    }
}
