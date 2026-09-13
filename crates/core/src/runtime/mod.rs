//! The agent runtime (tasks M00-11, M01-08): runs agents as tasks,
//! fans their events out to subscribers and keeps the registry of what
//! is running and of the conversation of every open session.
//!
//! [`run_agent`] records the start in the trace, spawns the loop
//! ([`agent`]) over the session's [`Conversation`] and returns a handle
//! at once; the RPC layer ([`rpc`]) forwards the handle's events to the
//! connection that asked. One agent runs per session at a time (a
//! second `agent.run` is a conflict), so the history the mentor sees
//! is always one straight line. Cancellation is a [`CancellationToken`]
//! per agent, a child of the daemon's shutdown token, so shutdown
//! cancels every in-flight mentor call and [`AgentRegistry::drain`]
//! lets them record their end before the trace store closes.

mod agent;
pub mod conversation;
pub mod hooks;
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

pub use agent::DEFAULT_WAIT;
pub use conversation::{CONTINUE_MESSAGE, Conversation, hash_tools};
pub use hooks::{CallContext, NoopHooks, StepHooks, ToolExecContext, ToolResultContext};

use crate::app::AppState;
use crate::permissions::EventSink;
use crate::trace::{AgentId, CallFilter, NewAgent, NewEvent, SessionId, kinds};

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

/// The permission prompter asks through the handle; "nobody listening"
/// is how it knows a run is headless.
impl EventSink for AgentHandle {
    fn emit(&self, event: Event) -> bool {
        self.events.send(event).is_ok()
    }
}

/// The conversation of one session, locked by the agent running on it.
pub type SharedConversation = Arc<tokio::sync::Mutex<Conversation>>;

/// The agents of one daemon, and the conversations of its sessions.
#[derive(Debug, Default)]
pub struct AgentRegistry {
    agents: Mutex<HashMap<AgentId, AgentHandle>>,
    conversations: Mutex<HashMap<SessionId, SharedConversation>>,
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

    /// The agent running on `session`, if one is.
    pub fn running_on(&self, session: &SessionId) -> Option<AgentHandle> {
        self.lock()
            .values()
            .find(|h| &h.session_id == session)
            .cloned()
    }

    /// The conversation of `session`, created empty on first use (a
    /// resumed session is loaded by M01-10).
    pub fn conversation(&self, session: &SessionId) -> SharedConversation {
        Arc::clone(
            self.conversations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(session.clone())
                .or_insert_with(|| {
                    Arc::new(tokio::sync::Mutex::new(Conversation::new(session.clone())))
                }),
        )
    }

    /// Forgets the conversation of `session` (its next run starts
    /// from an empty history).
    pub fn forget_conversation(&self, session: &SessionId) {
        self.conversations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session);
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

/// Starts an agent for `prompt` in `session` with the baseline hooks;
/// see [`run_agent_with`].
///
/// # Errors
/// See [`run_agent_with`].
pub async fn run_agent(
    state: &Arc<AppState>,
    session: SessionId,
    prompt: String,
    opts: RunOptions,
) -> Result<(AgentHandle, broadcast::Receiver<Event>), RpcError> {
    run_agent_with(state, session, prompt, opts, Arc::new(NoopHooks)).await
}

/// Starts an agent for `prompt` in `session`: records `agent.started`
/// and `user.message`, spawns the loop over the session's conversation
/// and returns the handle together with an event receiver subscribed
/// before the first event, so the caller misses nothing.
///
/// # Errors
/// Unknown session, an agent already running on it (`conflict`), or
/// the trace store cannot record the start.
pub async fn run_agent_with(
    state: &Arc<AppState>,
    session: SessionId,
    prompt: String,
    opts: RunOptions,
    hooks: Arc<dyn StepHooks>,
) -> Result<(AgentHandle, broadcast::Receiver<Event>), RpcError> {
    // Fail before anything is recorded when the session does not exist
    // or is busy.
    state.store().get_session(&session)?;
    if let Some(running) = state.agents().running_on(&session) {
        return Err(RpcError::conflict(format!(
            "agent {} is still running on session {session}",
            running.agent_id
        ))
        .with_details(serde_json::json!({ "agent_id": running.agent_id.to_string() })));
    }
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
    let conversation = registry.conversation(&session);
    // Two `agent.run` racing past the check above: the second waits
    // for the lock, so the histories still never interleave.
    registry.lock().insert(agent_id.clone(), handle.clone());
    info!(agent = %agent_id, session = %session, "agent started");

    let span = info_span!("agent", id = %agent_id);
    let task_state = Arc::clone(state);
    let task_handle = handle.clone();
    registry.tasks.spawn(
        async move {
            let event = {
                let mut conv = conversation.lock().await;
                seed_totals(&task_state, &mut conv);
                agent::execute(
                    &task_state,
                    &task_handle,
                    &prompt,
                    &opts,
                    &mut conv,
                    hooks.as_ref(),
                )
                .await
            };
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

/// A conversation that has no calls yet starts its running totals from
/// what the store has for the session (earlier daemon runs).
fn seed_totals(state: &AppState, conv: &mut Conversation) {
    if conv.last_usage().is_some() || conv.message_count() > 0 {
        return;
    }
    match state
        .store()
        .stats(&CallFilter::session_of(conv.session_id()))
    {
        Ok(t) if t.calls > 0 => conv.seed_totals(
            apprentice_api::types::Usage {
                input_tokens: t.input_tokens,
                output_tokens: t.output_tokens,
                cache_read_input_tokens: t.cache_read_tokens,
                cache_creation_input_tokens: t.cache_creation_tokens,
            },
            (t.unpriced_calls == 0).then_some(t.cost_micros),
        ),
        Ok(_) => {}
        Err(e) => tracing::debug!(error = %e, "cannot seed session totals"),
    }
}
