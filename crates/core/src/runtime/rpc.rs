//! `agent.run`, `agent.cancel` and `agent.subscribe`, and `prompt.show`.
//! Events go to the connection that asked, on a subscription named
//! after the agent; a connection that drops does not cancel the agent
//! (the CLI cancels explicitly on CTRL-C, the GUI may reattach with
//! `agent.subscribe`).

use std::path::Path;
use std::sync::Arc;

use apprentice_api::events::{AgentStatus, Event};
use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::{
    AgentCancel, AgentIdParams, AgentRun, AgentRunParams, AgentRunResult, AgentSubscribe,
    AgentSubscribeResult, Empty, PromptBlock, PromptShow, PromptShowParams, PromptShowResult,
};
use apprentice_api::server::{Connection, Router};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use super::prompt::{PROMPT_VERSION, SystemPrompt, build_system};
use super::run_agent;
use crate::app::AppState;
use crate::mentor::{MentorRequest, Message};
use crate::trace::{AgentId, RunStatus, SessionId};
use crate::workspace::Workspace;

/// Registers the three `agent.*` methods and `prompt.show`.
pub fn register(state: &Arc<AppState>, router: &mut Router) {
    let s = Arc::clone(state);
    router.add::<PromptShow, _, _>(move |_conn: Arc<Connection>, p: PromptShowParams| {
        let state = Arc::clone(&s);
        async move { prompt_show(&state, &p).await }
    });

    let s = Arc::clone(state);
    router.add::<AgentRun, _, _>(move |conn: Arc<Connection>, p: AgentRunParams| {
        let state = Arc::clone(&s);
        async move {
            let (handle, events) =
                run_agent(&state, p.session_id.into(), p.prompt, p.options).await?;
            let agent_id = handle.agent_id.to_string();
            tokio::spawn(forward(conn, agent_id.clone(), events));
            Ok(AgentRunResult {
                subscription: agent_id.clone(),
                agent_id,
            })
        }
    });

    let s = Arc::clone(state);
    router.add::<AgentCancel, _, _>(move |_conn: Arc<Connection>, p: AgentIdParams| {
        let state = Arc::clone(&s);
        async move {
            let id = AgentId::from(p.agent_id);
            if let Some(handle) = state.agents().get(&id) {
                debug!(agent = %id, "agent.cancel");
                handle.cancel.cancel();
                return Ok(Empty {});
            }
            // Known but already over: cancelling is idempotent.
            state.store().get_agent(&id)?;
            Ok(Empty {})
        }
    });

    let s = Arc::clone(state);
    router.add::<AgentSubscribe, _, _>(move |conn: Arc<Connection>, p: AgentIdParams| {
        let state = Arc::clone(&s);
        async move {
            let id = AgentId::from(p.agent_id);
            let subscription = id.to_string();
            if let Some(handle) = state.agents().get(&id) {
                let events = handle.subscribe();
                // Subscribed after the end: nothing more will arrive, so
                // deliver the terminal event instead of a silent stream.
                if let Some(finished) = handle.finished_event() {
                    return Ok(attach_finished(conn, &subscription, finished));
                }
                tokio::spawn(forward(conn, subscription.clone(), events));
                return Ok(AgentSubscribeResult {
                    subscription,
                    running: true,
                });
            }
            // Not running: the trace knows how it ended.
            let record = state.store().get_agent(&id)?;
            let status = match record.status {
                RunStatus::Ok => AgentStatus::Ok,
                RunStatus::Cancelled => AgentStatus::Cancelled,
                RunStatus::Error | RunStatus::Running => AgentStatus::Error,
            };
            let error = match record.status {
                RunStatus::Running => Some(RpcError::internal(
                    "agent ended without a record (daemon restarted?)",
                )),
                _ => None,
            };
            Ok(attach_finished(
                conn,
                &subscription,
                Event::AgentFinished {
                    agent_id: subscription.clone(),
                    status,
                    error,
                    truncated: false,
                },
            ))
        }
    });
}

/// `prompt.show` (task M01-09): the system blocks a session runs under
/// — its live conversation's when this daemon has one with a prompt set
/// and no agent holds it, else assembled now for its workspace — or
/// assembled for `workspace`; token count on request.
///
/// # Errors
/// Unknown session, a workspace that is not a directory, invalid
/// config.
pub async fn prompt_show(
    state: &Arc<AppState>,
    p: &PromptShowParams,
) -> Result<PromptShowResult, RpcError> {
    let mut from_session = None;
    let (workspace, live) = if let Some(id) = &p.session_id {
        let id = SessionId::from(id.clone());
        let session = state.store().get_session(&id)?;
        let workspace = match (&session.workspace_id, &session.workspace_path) {
            (Some(ws), _) => Some(state.workspaces().get(ws)?),
            (None, Some(path)) => Some(state.workspaces().open_root(Path::new(path))?),
            (None, None) => None,
        };
        let live = state.agents().loaded_conversation(&id).and_then(|conv| {
            let c = conv.try_lock().ok()?;
            (!c.system().is_empty()).then(|| SystemPrompt {
                version: c.prompt_version().unwrap_or(PROMPT_VERSION).to_owned(),
                blocks: c.system().to_vec(),
            })
        });
        if live.is_some() {
            from_session = Some(id.into_string());
        }
        (workspace, live)
    } else {
        let workspace = p
            .workspace
            .as_deref()
            .map(|root| state.workspaces().open_root(Path::new(root)))
            .transpose()?;
        (workspace, None)
    };
    let config = state
        .loader()
        .load(workspace.as_ref().map(|w| w.root()))?
        .config;
    let prompt = match live {
        Some(prompt) => prompt,
        None => build_system(workspace.as_ref(), &config).await,
    };
    let (tokens, token_error) = if p.count {
        match count(state, &prompt, &config.mentor.model).await {
            Ok(n) => (Some(n), None),
            Err(e) => (None, Some(e)),
        }
    } else {
        (None, None)
    };
    Ok(PromptShowResult {
        version: prompt.version,
        blocks: prompt
            .blocks
            .into_iter()
            .map(|b| PromptBlock {
                text: b.text,
                cache: true,
            })
            .collect(),
        session_id: from_session,
        workspace: workspace.as_deref().map(Workspace::root_string),
        tokens,
        token_error,
    })
}

/// `count_tokens` over the blocks and the shortest user message the
/// endpoint accepts.
async fn count(state: &Arc<AppState>, prompt: &SystemPrompt, model: &str) -> Result<u64, String> {
    let mentor = state.mentor().map_err(|e| e.to_string())?;
    let req = MentorRequest {
        model: model.to_owned(),
        max_tokens: 1,
        system: prompt.blocks.clone(),
        messages: vec![Message::user(".")],
        tools: Vec::new(),
        thinking: crate::mentor::Thinking::default(),
        effort: crate::mentor::Effort::High,
        metadata: None,
    };
    mentor.count_tokens(&req).await.map_err(|e| e.to_string())
}

/// Answers `agent.subscribe` for an agent that has ended: the terminal
/// event follows the reply (the router writes the reply first).
fn attach_finished(
    conn: Arc<Connection>,
    subscription: &str,
    finished: Event,
) -> AgentSubscribeResult {
    let sub = subscription.to_owned();
    tokio::spawn(async move {
        let _ = conn.notify(&sub, finished).await;
    });
    AgentSubscribeResult {
        subscription: subscription.to_owned(),
        running: false,
    }
}

/// Copies an agent's events to one connection until the terminal event
/// or the connection goes away.
async fn forward(
    conn: Arc<Connection>,
    subscription: String,
    mut events: broadcast::Receiver<Event>,
) {
    loop {
        let event = match events.recv().await {
            Ok(ev) => ev,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!(
                    conn = conn.id,
                    subscription,
                    dropped = n,
                    "subscriber lagged"
                );
                Event::warn(format!("{n} events dropped: the client fell behind"))
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        let terminal = event.is_terminal();
        if conn.notify(&subscription, event).await.is_err() {
            debug!(subscription, "subscriber gone");
            return;
        }
        if terminal {
            return;
        }
    }
}
