//! One agent turn: build the request, record it, call the mentor while
//! forwarding deltas, record the outcome. Every trace write goes through
//! the writer and is awaited, so by the time `agent.finished` reaches a
//! client the trace is committed.

use std::sync::Arc;

use apprentice_api::events::{AgentStatus, Event};
use apprentice_api::jsonrpc::RpcError;
use apprentice_api::types::RunOptions;
use serde_json::json;
use tracing::{debug, warn};

use super::AgentHandle;
use super::prompt::build_request;
use crate::app::AppState;
use crate::mentor::{MentorError, StopReason, StreamEvent};
use crate::stats::{micros_to_usd, price_call};
use crate::trace::{CallId, RunStatus, StepRef, kinds};

/// How the turn ended, before it is recorded.
struct Finish {
    status: AgentStatus,
    error: Option<RpcError>,
    truncated: bool,
}

impl Finish {
    fn ok(truncated: bool) -> Self {
        Self {
            status: AgentStatus::Ok,
            error: None,
            truncated,
        }
    }

    fn from_error(error: RpcError) -> Self {
        let status = if error.kind() == Some("cancelled") {
            AgentStatus::Cancelled
        } else {
            AgentStatus::Error
        };
        Self {
            status,
            error: Some(error),
            truncated: false,
        }
    }

    fn run_status(&self) -> RunStatus {
        match self.status {
            AgentStatus::Ok => RunStatus::Ok,
            AgentStatus::Cancelled => RunStatus::Cancelled,
            _ => RunStatus::Error,
        }
    }
}

/// Runs the turn and returns the `agent.finished` event, recorded.
pub(super) async fn execute(
    state: &Arc<AppState>,
    handle: &AgentHandle,
    prompt: &str,
    opts: &RunOptions,
) -> Event {
    let agent_id = handle.agent_id.clone();
    handle.emit(Event::AgentStarted {
        agent_id: agent_id.to_string(),
        session_id: handle.session_id.to_string(),
    });

    let finish = match turn(state, handle, prompt, opts).await {
        Ok(f) => f,
        Err(e) => Finish::from_error(e),
    };
    if let Some(e) = &finish.error {
        debug!(status = ?finish.status, error = %e.message, kind = e.kind(), "turn ended");
    }

    let status = finish.run_status();
    let error_json = finish
        .error
        .as_ref()
        .map(|e| serde_json::to_value(e).unwrap_or_else(|_| json!(e.message)));
    let id = agent_id.clone();
    if let Err(e) = state
        .writer()
        .run(move |store| store.finish_agent(&id, status, error_json))
        .await
    {
        warn!(error = %e, "cannot record agent.finished");
    }
    Event::AgentFinished {
        agent_id: agent_id.to_string(),
        status: finish.status,
        error: finish.error,
        truncated: finish.truncated,
    }
}

/// The single mentor turn of v0. Errors before the call (unknown
/// session, invalid config, no key) leave no step behind.
async fn turn(
    state: &Arc<AppState>,
    handle: &AgentHandle,
    prompt: &str,
    opts: &RunOptions,
) -> Result<Finish, RpcError> {
    let session = state.store().get_session(&handle.session_id)?;
    let workspace = session.workspace_path.as_deref().map(std::path::Path::new);
    let config = state.loader().load(workspace)?.config;
    let mentor = state.mentor()?;
    let req = build_request(&config, opts, prompt);
    let body = mentor.request_body(&req)?;

    let agent_id = handle.agent_id.clone();
    let step = state
        .writer()
        .run(move |store| store.start_step(&agent_id))
        .await?;
    let at = StepRef {
        session: handle.session_id.clone(),
        agent: handle.agent_id.clone(),
        step,
    };
    let call_id = CallId::generate();
    {
        let (at, call_id, req, body) = (at.clone(), call_id.clone(), req.clone(), body);
        state
            .writer()
            .run(move |store| store.record_mentor_request(&at, &call_id, &req, &body))
            .await?;
    }

    let agent = handle.agent_id.to_string();
    let mut on_event = |ev: StreamEvent| match ev {
        StreamEvent::TextDelta(text) => handle.emit(Event::AgentTextDelta {
            agent_id: agent.clone(),
            text,
        }),
        StreamEvent::ThinkingDelta(text) => handle.emit(Event::AgentThinkingDelta {
            agent_id: agent.clone(),
            text,
        }),
        _ => {}
    };
    let result = mentor
        .complete(&req, &mut on_event, handle.cancel.clone())
        .await;

    match result {
        Ok(resp) => {
            let cost_micros = price_call(&resp.model, &resp.usage, &config.pricing);
            let truncated = resp.stop_reason == StopReason::MaxTokens;
            let text = resp.text();
            let usage = resp.usage;
            let stop_reason = serde_json::to_value(&resp.stop_reason)
                .map_err(|e| RpcError::internal(e.to_string()))?;
            let message = at
                .event(kinds::ASSISTANT_MESSAGE)
                .payload(json!({
                    "call_id": call_id,
                    "text_len": text.len(),
                    "stop_reason": stop_reason,
                    "truncated": truncated,
                }))
                .blob_bytes(text, "text/plain; charset=utf-8");
            let step = at.step.clone();
            let call = call_id.clone();
            state
                .writer()
                .run(move |store| {
                    store.record_mentor_response(&at, &call, &resp, cost_micros)?;
                    store.append(message)?;
                    store.finish_step(&step, RunStatus::Ok)
                })
                .await?;
            handle.emit(Event::AgentUsage {
                agent_id: agent,
                call_id: call_id.to_string(),
                usage,
                cost_usd: cost_micros.map(micros_to_usd),
            });
            Ok(Finish::ok(truncated))
        }
        Err(err) => {
            let status = if matches!(err, MentorError::Cancelled) {
                RunStatus::Cancelled
            } else {
                RunStatus::Error
            };
            let step = at.step.clone();
            let error = state
                .writer()
                .run(move |store| {
                    store.record_mentor_error(&at, &call_id, &err, 0, true)?;
                    store.finish_step(&step, status)?;
                    Ok(RpcError::from(err))
                })
                .await?;
            Ok(Finish::from_error(error))
        }
    }
}
