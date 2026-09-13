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
//!
//! A session's conversation lives in memory while the daemon runs and
//! in `session_messages` for good (task M01-10): the first run after
//! a restart loads it back ([`load_conversation`]), repairs a turn a
//! crash left half-done, and carries on.

mod agent;
pub mod conversation;
pub mod hooks;
pub mod prompt;
mod revert;
pub(crate) mod rpc;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use apprentice_api::events::{AgentStatus, Event};
use apprentice_api::jsonrpc::RpcError;
pub use apprentice_api::types::RunOptions;
use serde_json::json;
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{Instrument, info, info_span, warn};

pub use agent::{DEFAULT_WAIT, stall_window};
pub use conversation::{CONTINUE_MESSAGE, Conversation, hash_tools};
pub use hooks::{CallContext, NoopHooks, StepHooks, ToolExecContext, ToolResultContext};
pub use prompt::{
    Host, MENTOR_SYSTEM_V1, PROMPT_VERSION, SystemPrompt, WorkspaceContext, assemble, build_system,
};
pub use rpc::prompt_show;

use crate::app::AppState;
use crate::mentor::{ContentBlock, Message, Role};
use crate::permissions::EventSink;
use crate::trace::{
    AgentId, CallFilter, NewAgent, NewEvent, NewMessage, SessionId, SessionStatus, TitleSource,
    first_prompt_title, kinds,
};

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
    /// When the agent last showed a sign of life, for the watchdog.
    activity: Arc<Activity>,
}

/// The stalled-agent watchdog's view of a run (task M01-15): the
/// moment of the last event, and whether the watchdog gave up on it.
#[derive(Debug)]
pub struct Activity {
    last: Mutex<Instant>,
    stalled: AtomicBool,
}

impl Default for Activity {
    fn default() -> Self {
        Self {
            last: Mutex::new(Instant::now()),
            stalled: AtomicBool::new(false),
        }
    }
}

impl Activity {
    /// Notes a sign of life now.
    pub fn touch(&self) {
        *self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
    }

    /// Time since the last sign of life.
    pub fn idle(&self) -> Duration {
        self.last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .elapsed()
    }

    /// The watchdog ended the run.
    pub fn mark_stalled(&self) {
        self.stalled.store(true, Ordering::SeqCst);
    }

    pub fn is_stalled(&self) -> bool {
        self.stalled.load(Ordering::SeqCst)
    }
}

impl AgentHandle {
    /// The run's activity record (see [`Activity`]).
    pub fn activity(&self) -> &Arc<Activity> {
        &self.activity
    }

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
        self.activity.touch();
        let _ = self.events.send(event);
    }
}

/// The permission prompter asks through the handle; "nobody listening"
/// is how it knows a run is headless.
impl EventSink for AgentHandle {
    fn emit(&self, event: Event) -> bool {
        self.activity.touch();
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

    /// The conversation of `session` when this daemon holds it (see
    /// [`load_conversation`] for the one that brings it in).
    pub fn loaded_conversation(&self, session: &SessionId) -> Option<SharedConversation> {
        self.conversations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session)
            .cloned()
    }

    /// Keeps `conversation` for `session`; when one arrived first (two
    /// loads racing) that one wins and is returned.
    pub fn insert_conversation(
        &self,
        session: SessionId,
        conversation: Conversation,
    ) -> SharedConversation {
        Arc::clone(
            self.conversations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(session)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(conversation))),
        )
    }

    /// Runs `task` under the registry's tracker, so shutdown waits for
    /// it like for an agent (the title generator uses this).
    pub fn spawn<F>(&self, task: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.tasks.spawn(task);
    }

    /// Forgets the conversation of `session` (its next run loads it
    /// from the store again, or finds nothing there).
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

/// Starts an agent for `prompt` in `session`: records `agent.started`,
/// appends the user turn to the conversation (loading it from the
/// store first when this daemon has not seen the session) together
/// with its `user.message`, spawns the loop and returns the handle
/// together with an event receiver subscribed before the first event,
/// so the caller misses nothing. The first run names an untitled
/// session after its prompt; the first answered run asks the title
/// model for a better one (`sessions.auto_title`).
///
/// # Errors
/// Unknown or deleted session, an agent already running on it
/// (`conflict`), a stored history the API would reject, or the trace
/// store cannot record the start.
pub async fn run_agent_with(
    state: &Arc<AppState>,
    session: SessionId,
    prompt: String,
    opts: RunOptions,
    hooks: Arc<dyn StepHooks>,
) -> Result<(AgentHandle, broadcast::Receiver<Event>), RpcError> {
    // Fail before anything is recorded when the session does not exist
    // or is busy.
    let record = state.store().get_session(&session)?;
    if record.status == SessionStatus::Deleted {
        return Err(RpcError::conflict(format!(
            "session {session} was deleted; its conversation is gone"
        )));
    }
    if let Some(running) = state.agents().running_on(&session) {
        return Err(RpcError::conflict(format!(
            "agent {} is still running on session {session}",
            running.agent_id
        ))
        .with_details(json!({ "agent_id": running.agent_id.to_string() })));
    }
    let conversation = load_conversation(state, &session).await?;
    let new_agent = NewAgent {
        options: serde_json::to_value(&opts).map_err(|e| RpcError::internal(e.to_string()))?,
        ..NewAgent::main(session.clone(), prompt.clone())
    };
    let agent_id = state
        .writer()
        .run(move |store| store.start_agent(&new_agent))
        .await?;

    let (events, receiver) = broadcast::channel(EVENT_BUFFER);
    let (finished_tx, finished) = watch::channel(None);
    let handle = AgentHandle {
        agent_id: agent_id.clone(),
        session_id: session.clone(),
        cancel: state.shutdown().child_token(),
        events,
        finished,
        activity: Arc::new(Activity::default()),
    };
    let registry = state.agents();
    // From here on a second `agent.run` on the session is a conflict;
    // the lock below is only against a reader (`prompt.show`).
    registry.lock().insert(agent_id.clone(), handle.clone());

    // The user turn, in memory and in the store as one transaction
    // with its event. A provisional title for a session without one.
    let title = (record.title_source.is_none() && record.message_count == 0)
        .then(|| first_prompt_title(&prompt))
        .filter(|t| !t.is_empty());
    let recorded = {
        let mut conv = conversation.lock().await;
        conv.push_user_text(prompt.as_str());
        let row = user_row(&conv, &agent_id);
        let (session, agent, text) = (session.clone(), agent_id.clone(), prompt.clone());
        state
            .writer()
            .run(move |store| {
                if let Some(title) = &title {
                    store.set_session_title(&session, Some(title), TitleSource::Prompt)?;
                }
                store.append_with_message(
                    NewEvent::new(session.clone(), kinds::USER_MESSAGE)
                        .agent(agent)
                        .payload(json!({ "text_len": text.len() }))
                        .blob_bytes(text, "text/plain; charset=utf-8"),
                    &row,
                )
            })
            .await
    };
    if let Err(e) = recorded {
        registry.remove(&agent_id);
        return Err(e.into());
    }
    info!(agent = %agent_id, session = %session, "agent started");

    let span = info_span!("agent", id = %agent_id);
    let task_state = Arc::clone(state);
    let task_handle = handle.clone();
    registry.tasks.spawn(
        async move {
            let (event, answer) = {
                let mut conv = conversation.lock().await;
                let event =
                    agent::execute(&task_state, &task_handle, &opts, &mut conv, hooks.as_ref())
                        .await;
                (event, last_answer(&conv))
            };
            // `execute` has recorded `agent.finished`, so `agent.subscribe`
            // finds the end in the store once the registry drops the agent
            // and in the handle (set before the event is emitted) until.
            task_state.agents().remove(&task_handle.agent_id);
            let _ = finished_tx.send(Some(event.clone()));
            let ok = matches!(
                event,
                Event::AgentFinished {
                    status: AgentStatus::Ok,
                    ..
                }
            );
            task_handle.emit(event);
            if ok && let Some(answer) = answer {
                crate::sessions::title::maybe_generate(
                    &task_state,
                    task_handle.session_id.clone(),
                    task_handle.agent_id.clone(),
                    prompt,
                    answer,
                );
            }
        }
        .instrument(span),
    );
    Ok((handle, receiver))
}

/// The conversation of `session`: the one this daemon holds, else the
/// stored one, loaded and checked. A trailing assistant turn whose
/// tool calls were never answered (a crash between the call and the
/// tools) is dropped here and in the store, with `session.repaired`
/// in the trace. Running totals start from the session's recorded
/// calls.
///
/// # Errors
/// Unknown session; a stored history that breaks the API's rules
/// beyond that repair (`internal`).
pub async fn load_conversation(
    state: &Arc<AppState>,
    session: &SessionId,
) -> Result<SharedConversation, RpcError> {
    if let Some(conv) = state.agents().loaded_conversation(session) {
        return Ok(conv);
    }
    let record = state.store().get_session(session)?;
    let rows = state.store().session_messages(session, 0, None)?;
    let messages = rows
        .into_iter()
        .map(|r| Message {
            role: r.role,
            content: r.content,
        })
        .collect();
    let mut conv = Conversation::load(session.clone(), messages).with_tools_hash(record.tools_hash);
    if let Some(dropped) = conv.repair() {
        let keep = conv.message_count() as u64;
        let ids: Vec<String> = dropped
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        warn!(
            session = %session,
            dropped_seq = keep + 1,
            tool_uses = ?ids,
            "dropped an assistant turn whose tool calls had no results"
        );
        let sid = session.clone();
        state
            .writer()
            .run(move |store| {
                store.truncate_session_messages(&sid, keep)?;
                store.append(
                    NewEvent::new(sid.clone(), kinds::SESSION_REPAIRED).payload(json!({
                        "dropped_seq": keep + 1,
                        "tool_use_ids": ids,
                    })),
                )
            })
            .await?;
    }
    if let Err(reason) = conv.validate() {
        return Err(RpcError::internal(format!(
            "session {session}: the stored conversation is not one the API accepts ({reason});              delete the session or export it for inspection"
        ))
        .with_details(json!({ "reason": "invalid_history" })));
    }
    if record.message_count > 0 {
        seed_totals(state, &mut conv);
    }
    Ok(state.agents().insert_conversation(session.clone(), conv))
}

/// The last message as a store row (task M01-10 keeps `seq` equal to
/// the message's position).
fn user_row(conv: &Conversation, agent: &AgentId) -> NewMessage {
    let (seq, msg) = conv.last_row().expect("the user turn was just pushed");
    NewMessage {
        session: conv.session_id().clone(),
        seq,
        role: msg.role,
        content: msg.content.clone(),
        agent: Some(agent.clone()),
        step: None,
    }
}

/// The text of the final assistant turn, when the conversation ends
/// on one.
fn last_answer(conv: &Conversation) -> Option<String> {
    let last = conv.messages().last()?;
    (last.role == Role::Assistant).then(|| {
        last.content
            .iter()
            .filter_map(ContentBlock::as_text)
            .collect::<String>()
    })
}

/// A loaded conversation starts its running totals from what the
/// store has for the session (earlier daemon runs).
fn seed_totals(state: &AppState, conv: &mut Conversation) {
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
            t.calls,
        ),
        Ok(_) => {}
        Err(e) => tracing::debug!(error = %e, "cannot seed session totals"),
    }
}
