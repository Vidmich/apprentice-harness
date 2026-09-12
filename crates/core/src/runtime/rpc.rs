//! `agent.run`, `agent.cancel` and `agent.subscribe`. Events go to the
//! connection that asked, on a subscription named after the agent; a
//! connection that drops does not cancel the agent (the CLI cancels
//! explicitly on CTRL-C, the GUI may reattach with `agent.subscribe`).

use std::sync::Arc;

use apprentice_api::events::{AgentStatus, Event};
use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::{
    AgentCancel, AgentIdParams, AgentRun, AgentRunParams, AgentRunResult, AgentSubscribe,
    AgentSubscribeResult, Empty,
};
use apprentice_api::server::{Connection, Router};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use super::run_agent;
use crate::app::AppState;
use crate::trace::{AgentId, RunStatus};

/// Registers the three `agent.*` methods.
pub fn register(state: &Arc<AppState>, router: &mut Router) {
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
