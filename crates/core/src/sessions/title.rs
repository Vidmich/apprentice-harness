//! Session titles (task M01-10). A session without a title gets the
//! first line of its first prompt at the first run (the runtime does
//! that in the same transaction as the user turn); after the first
//! answered run, [`maybe_generate`] asks `mentor.title_model` for a
//! short one — a call like any other in the trace (kind `title`, on a
//! step of the agent that just finished) and in the stats — unless the
//! user named the session or `sessions.auto_title` is off.

use std::sync::Arc;

use serde_json::json;
use tracing::{debug, info, warn};

use crate::app::AppState;
use crate::mentor::{Effort, MentorError, MentorRequest, Message, SystemBlock, Thinking};
use crate::stats::price_call;
use crate::trace::{
    AgentId, CallId, CallKind, RunStatus, SessionId, StepRef, TitleSource, TraceError, kinds,
};

/// What the title model is told.
pub const TITLE_SYSTEM: &str = "You name coding sessions. Reply with a title of at most eight \
    words for the conversation you are given: what the user wanted, in plain words. No quotes, \
    no trailing period, nothing but the title.";
/// Output cap of the title call.
pub const TITLE_MAX_TOKENS: u32 = 30;
/// Characters of the prompt and of the answer the title model sees.
pub const INPUT_CHARS: usize = 2_000;
/// Longest title kept.
pub const TITLE_CHARS: usize = 80;

/// Starts the title task for `session` when it is due: the run ended
/// with an answer, the config allows it, and the title is still the
/// provisional one. Runs under the agent registry so shutdown waits
/// for it.
pub fn maybe_generate(
    state: &Arc<AppState>,
    session: SessionId,
    agent: AgentId,
    prompt: String,
    answer: String,
) {
    let config = match state.config() {
        Ok(c) => c,
        Err(e) => {
            debug!(error = %e, "no config; skipping the session title");
            return;
        }
    };
    if !config.sessions.auto_title {
        return;
    }
    match state.store().get_session(&session) {
        Ok(record) if record.title_source == Some(TitleSource::Prompt) => {}
        Ok(_) => return,
        Err(e) => {
            debug!(error = %e, "cannot read the session; skipping the title");
            return;
        }
    }
    let task_state = Arc::clone(state);
    state.agents().spawn(async move {
        if let Err(e) = generate(&task_state, &session, &agent, &prompt, &answer).await {
            warn!(session = %session, error = %e, "session title not generated");
        }
    });
}

/// The request the title model gets: the system line and one user
/// message with the prompt and the answer, each cut to
/// [`INPUT_CHARS`].
pub fn request(model: &str, prompt: &str, answer: &str) -> MentorRequest {
    MentorRequest {
        model: model.to_owned(),
        max_tokens: TITLE_MAX_TOKENS,
        system: vec![SystemBlock::new(TITLE_SYSTEM)],
        messages: vec![Message::user(format!(
            "User:\n{}\n\nAssistant:\n{}",
            cut(prompt, INPUT_CHARS),
            cut(answer, INPUT_CHARS)
        ))],
        tools: Vec::new(),
        thinking: Thinking::Disabled,
        effort: Effort::Low,
        metadata: None,
    }
}

/// One title call, recorded on a fresh step of `agent`; the title is
/// set unless the user renamed the session meanwhile.
async fn generate(
    state: &Arc<AppState>,
    session: &SessionId,
    agent: &AgentId,
    prompt: &str,
    answer: &str,
) -> Result<(), String> {
    let config = state.config().map_err(|e| e.to_string())?;
    let mentor = state.mentor().map_err(|e| e.to_string())?;
    let req = request(&config.mentor.title_model, prompt, answer);
    let body = mentor.request_body(&req).map_err(|e| e.to_string())?;
    let call_id = CallId::generate();
    let at = {
        let step_agent = agent.clone();
        let step = state
            .writer()
            .run(move |store| store.start_step(&step_agent))
            .await
            .map_err(|e| e.to_string())?;
        StepRef {
            session: session.clone(),
            agent: agent.clone(),
            step,
        }
    };
    {
        let (at, call_id, req) = (at.clone(), call_id.clone(), req.clone());
        state
            .writer()
            .run(move |store| {
                store.record_mentor_request(&at, &call_id, CallKind::Title, &req, &body, None)
            })
            .await
            .map_err(|e| e.to_string())?;
    }
    let result = mentor
        .complete(&req, &mut |_| {}, state.shutdown().child_token())
        .await;
    let resp = match result {
        Ok(resp) => resp,
        Err(err) => {
            let (at, call_id) = (at.clone(), call_id.clone());
            let status = if matches!(err, MentorError::Cancelled) {
                RunStatus::Cancelled
            } else {
                RunStatus::Error
            };
            let message = err.to_string();
            let _ = state
                .writer()
                .run(move |store| {
                    store.record_mentor_error(&at, &call_id, &err, 0, true)?;
                    store.finish_step(&at.step, status)
                })
                .await;
            return Err(message);
        }
    };
    let cost_micros = price_call(&resp.model, &resp.usage, &config.pricing);
    let title = clean(&resp.text());
    let (rec_at, rec_call, rec_resp) = (at.clone(), call_id.clone(), resp.clone());
    state
        .writer()
        .run(move |store| {
            store.record_mentor_response(&rec_at, &rec_call, &rec_resp, cost_micros)?;
            store.finish_step(&rec_at.step, RunStatus::Ok)
        })
        .await
        .map_err(|e| e.to_string())?;
    let Some(title) = title else {
        debug!(session = %session, "the title model answered nothing usable");
        return Ok(());
    };
    let (sid, event_at, event_title) = (session.clone(), at.clone(), title.clone());
    let set = state
        .writer()
        .run(move |store| {
            let set = store.set_session_title(&sid, Some(&event_title), TitleSource::Generated)?;
            if set {
                store.append(event_at.event(kinds::SESSION_TITLE).payload(json!({
                    "title": event_title,
                    "source": TitleSource::Generated,
                    "call_id": call_id,
                })))?;
            }
            Ok::<_, TraceError>(set)
        })
        .await
        .map_err(|e| e.to_string())?;
    if set {
        info!(session = %session, title, "session titled");
    } else {
        debug!(session = %session, "the user renamed the session meanwhile; title kept");
    }
    Ok(())
}

/// The model's answer as a title: the first non-empty line, quotes
/// and a trailing period stripped, cut to [`TITLE_CHARS`]. `None`
/// when nothing is left.
pub fn clean(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '“' || c == '”')
        .trim_end_matches('.')
        .trim();
    if line.is_empty() {
        return None;
    }
    Some(cut(line, TITLE_CHARS))
}

/// The first `max` characters, with an ellipsis when something was
/// cut.
fn cut(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{}…", head.trim_end())
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_cleaned_into_titles() {
        assert_eq!(
            clean("\"Fix the failing tests.\"\n"),
            Some("Fix the failing tests".into())
        );
        assert_eq!(
            clean("\n  Add a hello module  "),
            Some("Add a hello module".into())
        );
        assert_eq!(clean("..."), None);
        assert_eq!(clean(""), None);
        let long = "w".repeat(100);
        let t = clean(&long).unwrap();
        assert_eq!(t.chars().count(), TITLE_CHARS + 1);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn the_request_is_small_and_thinking_free() {
        let req = request("claude-haiku-4-5-20251001", &"p".repeat(3000), "a");
        assert_eq!(req.max_tokens, TITLE_MAX_TOKENS);
        assert_eq!(req.thinking, Thinking::Disabled);
        assert!(req.tools.is_empty());
        let text = req.messages[0].content[0].as_text().unwrap();
        assert!(text.starts_with("User:\n"));
        assert!(text.contains("…\n\nAssistant:\na"), "{text}");
        assert!(text.len() < INPUT_CHARS + 100);
    }
}
