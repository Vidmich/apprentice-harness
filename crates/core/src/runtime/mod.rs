//! The agent runtime (task M00-11): runs agents as tasks, fans their
//! events out to subscribers and keeps the registry of what is running.
//!
//! v0 is a single mentor turn without tools: [`run_agent`] records the
//! start in the trace, spawns the turn ([`agent`]) and returns a handle
//! at once; the RPC layer ([`rpc`]) forwards the handle's events to the
//! connection that asked. Cancellation is a [`CancellationToken`] per
//! agent, a child of the daemon's shutdown token, so shutdown cancels
//! every in-flight mentor call and [`AgentRegistry::drain`] lets them
//! record their end before the trace store closes.

mod agent;
pub mod prompt;
pub(crate) mod rpc;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use apprentice_api::events::{AgentStatus, Event};
use apprentice_api::jsonrpc::RpcError;
pub use apprentice_api::types::RunOptions;
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{Instrument, info, info_span};

use crate::app::AppState;
use crate::trace::{AgentId, NewAgent, NewEvent, SessionId, kinds};

/// Events buffered per agent for a subscriber that falls behind; a
/// subscriber that lags further gets a `log` event naming the gap.
pub const EVENT_BUFFER: usize = 1024;

/// One agent as the RPC layer sees it. Lives in the registry while the
/// agent runs; clones stay valid after, reporting the final event.
#[derive(Debug, Clone)]
pub struct AgentHandle {
    pub agent_id: AgentId,
    pub session_id: SessionId,
    /// Cancels the agent's mentor call; the agent still records its end.
    pub cancel: CancellationToken,
    events: broadcast::Sender<Event>,
    /// The `agent.finished` event once there is one.
    finished: watch::Receiver<Option<Event>>,
}

impl AgentHandle {
    /// Future events of this agent (nothing is replayed). Subscribe, then
    /// check [`Self::finished_event`]: an agent that ended before the
    /// subscription will send nothing more.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// The terminal event, once the agent has ended.
    pub fn finished_event(&self) -> Option<Event> {
        self.finished.borrow().clone()
    }

    pub fn is_running(&self) -> bool {
        self.finished.borrow().is_none()
    }

    /// Terminal status, once the agent has ended.
    pub fn status(&self) -> Option<AgentStatus> {
        match self.finished_event() {
            Some(Event::AgentFinished { status, .. }) => Some(status),
            _ => None,
        }
    }

    /// Resolves with the terminal event.
    pub async fn finished(&self) -> Event {
        let mut rx = self.finished.clone();
        let _ = rx.wait_for(Option::is_some).await;
        // The sender is dropped only after the terminal event is set.
        rx.borrow()
            .clone()
            .expect("agent task sets the final event before ending")
    }

    fn emit(&self, event: Event) {
        // No subscriber is fine: the trace is the record, events are a
        // live view.
        let _ = self.events.send(event);
    }
}

/// The agents of one daemon.
#[derive(Debug, Default)]
pub struct AgentRegistry {
    agents: Mutex<HashMap<AgentId, AgentHandle>>,
    tasks: TaskTracker,
    /// When the last agent finished; the idle timer counts from here.
    last_finished: Mutex<Option<Instant>>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<AgentId, AgentHandle>> {
        self.agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The handle of a running agent.
    pub fn get(&self, id: &AgentId) -> Option<AgentHandle> {
        self.lock().get(id).cloned()
    }

    /// Number of agents still running.
    pub fn running(&self) -> usize {
        self.lock().len()
    }

    /// When the last agent finished, if any has.
    pub fn last_finished(&self) -> Option<Instant> {
        *self
            .last_finished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn remove(&self, id: &AgentId) {
        self.lock().remove(id);
        *self
            .last_finished
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
    }

    /// Waits for every agent task to end (they are cancelled by the
    /// shutdown token; this lets them record their end). Returns `false`
    /// when `timeout` passed first.
    pub async fn drain(&self, timeout: Duration) -> bool {
        // Closing only makes `wait` return once the current tasks are
        // gone; the tracker keeps accepting tasks.
        self.tasks.close();
        let drained = tokio::time::timeout(timeout, self.tasks.wait())
            .await
            .is_ok();
        self.tasks.reopen();
        drained
    }
}

/// Starts an agent for `prompt` in `session`: records `agent.started`
/// and `user.message`, spawns the turn and returns the handle together
/// with an event receiver subscribed before the first event, so the
/// caller misses nothing.
///
/// # Errors
/// Unknown session, or the trace store cannot record the start.
pub async fn run_agent(
    state: &Arc<AppState>,
    session: SessionId,
    prompt: String,
    opts: RunOptions,
) -> Result<(AgentHandle, broadcast::Receiver<Event>), RpcError> {
    // Fail before anything is recorded when the session does not exist.
    state.store().get_session(&session)?;
    let new_agent = NewAgent {
        options: serde_json::to_value(&opts).map_err(|e| RpcError::internal(e.to_string()))?,
        ..NewAgent::main(session.clone(), prompt.clone())
    };
    let agent_id = state
        .writer()
        .run(move |store| store.start_agent(&new_agent))
        .await?;
    state
        .writer()
        .append(
            NewEvent::new(session.clone(), kinds::USER_MESSAGE)
                .agent(agent_id.clone())
                .payload(serde_json::json!({ "text_len": prompt.len() }))
                .blob_bytes(prompt.clone(), "text/plain; charset=utf-8"),
        )
        .await?;

    let (events, receiver) = broadcast::channel(EVENT_BUFFER);
    let (finished_tx, finished) = watch::channel(None);
    let handle = AgentHandle {
        agent_id: agent_id.clone(),
        session_id: session.clone(),
        cancel: state.shutdown().child_token(),
        events,
        finished,
    };
    let registry = state.agents();
    registry.lock().insert(agent_id.clone(), handle.clone());
    info!(agent = %agent_id, session = %session, "agent started");

    let span = info_span!("agent", id = %agent_id);
    let task_state = Arc::clone(state);
    let task_handle = handle.clone();
    registry.tasks.spawn(
        async move {
            let event = agent::execute(&task_state, &task_handle, &prompt, &opts).await;
            // `execute` has recorded `agent.finished`, so `agent.subscribe`
            // finds the end in the store once the registry drops the agent
            // and in the handle (set before the event is emitted) until.
            task_state.agents().remove(&task_handle.agent_id);
            let _ = finished_tx.send(Some(event.clone()));
            task_handle.emit(event);
        }
        .instrument(span),
    );
    Ok((handle, receiver))
}
