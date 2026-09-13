//! The agent loop (task M01-08): mentor call → tools → mentor call,
//! until the mentor ends the turn. Every step is one mentor call plus
//! the tool executions it asked for (SPEC §9); every request is
//! serialised once and the bytes go to the trace and to the wire; every
//! trace write is awaited, so by the time `agent.finished` reaches a
//! client the record is committed.
//!
//! What ends a run: `end_turn` (ok), a refusal, the iteration or
//! context guards, a mentor error the retries did not cure, or
//! cancellation. A tool failure never does — it is an error result the
//! mentor reads. The hook points ([`StepHooks`]) are called at every
//! step and do nothing until M03.
//!
//! Every message appended to the conversation is written through to
//! `session_messages` as it happens (task M01-10): the assistant turn
//! with its `assistant.message`, the tool results after the step, so a
//! daemon that dies mid-run leaves a history the next one resumes.
//!
//! Outcome signals (task M01-15) are recorded as they arise: the
//! `tests` / `build` result of a tool run right after the step's
//! tools, the `files_changed` of the run at its end, an `error` when
//! the loop stops itself, a `reverted` for the previous run when the
//! start snapshot shows its changes undone. A watchdog ends a run that
//! shows no sign of life for [`stall_window`] as `error: stalled`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use apprentice_api::events::{AgentStatus, Event, StepPhase};
use apprentice_api::jsonrpc::RpcError;
use apprentice_api::types::{PermissionMode, RunOptions};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::AgentHandle;
use super::conversation::{CONTINUE_MESSAGE, Conversation};
use super::hooks::{CallContext, StepHooks, ToolExecContext, ToolResultContext};
use super::prompt::build_system;
use crate::app::AppState;
use crate::config::Config;
use crate::mentor::{
    ContentBlock, Mentor, MentorError, MentorResponse, StopReason, StreamEvent, ToolResultContent,
};
use crate::outcomes::{self, Outcome};
use crate::permissions::{AgentPrompter, Engine, EventSink, PermissionGate, Prompter};
use crate::stats::{micros_to_usd, price_call};
use crate::tools::{Executed, Executor, SeenFiles, ToolCall, ToolProgress, ToolResultKind};
use crate::trace::{
    CallId, CallKind, NewEvent, NewMessage, RunStatus, SessionRecord, StepRef, format_ts, kinds,
};
use crate::workspace::{Snapshot, SnapshotPhase, Workspace};

/// How long to wait for an overload (or a rate limit without
/// `retry-after`) to pass before the next attempt.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(30);

/// Added to the user turn of a resumed session whose tool set differs
/// from the one its history was made with.
pub const TOOLS_CHANGED_NOTE: &str = "[note: tool set changed since this session started]";

/// How the run ended, before it is recorded.
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

    fn cancelled() -> Self {
        Self::from_error(RpcError::cancelled())
    }

    /// Stopped by the loop itself: `kind` is `refusal`,
    /// `max_iterations` or `context_limit`.
    fn stopped(kind: &str, message: String, details: Value) -> Self {
        Self::from_error(RpcError::agent_stopped(kind, message).with_details(details))
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

/// Runs the loop on `conversation`, whose last message is the user
/// turn of this run, and returns the `agent.finished` event, recorded.
pub(super) async fn execute(
    state: &Arc<AppState>,
    handle: &AgentHandle,
    opts: &RunOptions,
    conversation: &mut Conversation,
    hooks: &dyn StepHooks,
) -> Event {
    let agent_id = handle.agent_id.clone();
    handle.emit(Event::AgentStarted {
        agent_id: agent_id.to_string(),
        session_id: handle.session_id.to_string(),
    });

    let finish = match Run::setup(state, handle, opts, conversation, hooks).await {
        Ok(mut run) => {
            let watchdog = run.spawn_watchdog();
            let start = run.snapshot(SnapshotPhase::Start).await;
            if let Some(start) = &start {
                run.check_reverted(start).await;
            }
            let mut finish = match run.run_loop(conversation).await {
                Ok(f) => f,
                Err(e) => Finish::from_error(e),
            };
            if let Some(w) = watchdog {
                w.abort();
            }
            if handle.activity().is_stalled() {
                let window = stall_window(&run.config);
                let details = json!({ "window_s": window.as_secs() });
                run.record_outcome(&Outcome::error("stalled", &details), None)
                    .await;
                finish = Finish::stopped(
                    "stalled",
                    format!(
                        "no sign of life for {} s; the run was ended by the watchdog",
                        window.as_secs()
                    ),
                    details,
                );
            }
            run.finish_snapshot(start).await;
            finish
        }
        // Nothing recorded yet beyond the start: unknown workspace,
        // invalid config, no key.
        Err(e) => Finish::from_error(e),
    };
    if let Some(e) = &finish.error {
        debug!(status = ?finish.status, error = %e.message, kind = e.kind(), "run ended");
    }
    // A run that ended between a call and its tools (a trace failure
    // in the tool step) leaves tool calls without results: not a
    // history the next request may carry.
    if let Some(dropped) = conversation.repair() {
        let keep = conversation.message_count() as u64;
        let ids: Vec<String> = dropped
            .content
            .iter()
            .filter_map(|b| match b {
                crate::mentor::ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        warn!(
            dropped_seq = keep + 1,
            ?ids,
            "dropped an assistant turn whose tool calls had no results"
        );
        let session = handle.session_id.clone();
        let agent = agent_id.clone();
        if let Err(e) = state
            .writer()
            .run(move |store| {
                store.truncate_session_messages(&session, keep)?;
                store.append(
                    NewEvent::new(session.clone(), kinds::SESSION_REPAIRED)
                        .agent(agent)
                        .payload(json!({ "dropped_seq": keep + 1, "tool_use_ids": ids })),
                )
            })
            .await
        {
            warn!(error = %e, "cannot record session.repaired");
        }
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

/// Records what the session runs under (task M01-10): the prompt
/// version and tool-set hash at the first run; a change since then as
/// `session.prefix_changed` (a cache miss once) with a note to the
/// mentor when the tools differ, since the history may name tools that
/// are gone.
async fn record_prefix(
    state: &Arc<AppState>,
    handle: &AgentHandle,
    session: &SessionRecord,
    conversation: &mut Conversation,
) -> Result<(), RpcError> {
    let prompt_version = conversation.prompt_version().unwrap_or_default().to_owned();
    let tools_hash = conversation.tools_hash().unwrap_or_default().to_owned();
    let (stored_version, stored_hash) = (&session.prompt_version, &session.tools_hash);
    if stored_version.is_none() && stored_hash.is_none() {
        let (id, v, h) = (session.id.clone(), prompt_version, tools_hash);
        state
            .writer()
            .run(move |store| store.set_session_prefix(&id, &v, &h))
            .await?;
        return Ok(());
    }
    let mut changed = serde_json::Map::new();
    if stored_version.as_deref() != Some(prompt_version.as_str()) {
        changed.insert(
            "prompt_version".into(),
            json!({ "from": stored_version, "to": prompt_version }),
        );
    }
    let tools_differ = stored_hash.as_deref() != Some(tools_hash.as_str());
    if tools_differ {
        changed.insert(
            "tools_hash".into(),
            json!({ "from": stored_hash, "to": tools_hash }),
        );
    }
    if changed.is_empty() {
        return Ok(());
    }
    info!(session = %session.id, changed = ?changed.keys().collect::<Vec<_>>(), "session prefix changed");
    let note_row = tools_differ.then(|| {
        conversation.push_user_text(TOOLS_CHANGED_NOTE);
        let (seq, msg) = conversation
            .last_row()
            .expect("a user turn was just pushed");
        NewMessage {
            session: session.id.clone(),
            seq,
            role: msg.role,
            content: msg.content.clone(),
            agent: Some(handle.agent_id.clone()),
            step: None,
        }
    });
    let (id, v, h) = (session.id.clone(), prompt_version, tools_hash);
    let agent = handle.agent_id.clone();
    state
        .writer()
        .run(move |store| {
            store.set_session_prefix(&id, &v, &h)?;
            if let Some(row) = &note_row {
                store.put_session_message(row)?;
            }
            store.append(
                NewEvent::new(id, kinds::SESSION_PREFIX_CHANGED)
                    .agent(agent)
                    .payload(Value::Object(changed)),
            )
        })
        .await?;
    Ok(())
}

/// How long a run may go without an event before the watchdog ends
/// it: the longest configured wait (the mentor's timeout, the execute
/// tools' timeout, the permission prompt's) plus
/// `runtime.stall_grace_s`; zero when the grace is zero (off).
pub fn stall_window(config: &Config) -> Duration {
    if config.runtime.stall_grace_s == 0 {
        return Duration::ZERO;
    }
    let longest = config
        .mentor
        .timeout_s
        .max(config.tools.timeout_s.execute)
        .max(config.permissions.ask_timeout_s);
    Duration::from_secs(longest.saturating_add(config.runtime.stall_grace_s))
}

/// Everything one run needs, resolved once before the first step.
struct Run<'a> {
    state: &'a Arc<AppState>,
    handle: &'a AgentHandle,
    hooks: &'a dyn StepHooks,
    config: Config,
    mentor: Arc<dyn Mentor>,
    workspace: Option<Arc<Workspace>>,
    engine: Arc<Engine>,
    prompter: Arc<dyn Prompter>,
    sink: Arc<dyn EventSink>,
    mode: PermissionMode,
    /// Files read by this agent, shared by its steps.
    seen: Arc<SeenFiles>,
    agent: String,
    /// The soft context warning went out.
    warned_context: bool,
    /// The `max_tokens` continuation was used.
    continued: bool,
}

impl<'a> Run<'a> {
    async fn setup(
        state: &'a Arc<AppState>,
        handle: &'a AgentHandle,
        opts: &RunOptions,
        conversation: &mut Conversation,
        hooks: &'a dyn StepHooks,
    ) -> Result<Self, RpcError> {
        let session = state.store().get_session(&handle.session_id)?;
        let workspace = match (&session.workspace_id, &session.workspace_path) {
            (Some(id), _) => Some(state.workspaces().get(id)?),
            (None, Some(path)) => Some(state.workspaces().open_root(Path::new(path))?),
            (None, None) => None,
        };
        let config = state
            .loader()
            .load(workspace.as_ref().map(|w| w.root()))?
            .config;
        let mentor = state.mentor()?;
        let mode = opts
            .permission_mode
            .unwrap_or(config.permissions.default_mode);
        let engine = Arc::new(Engine::new(
            state.paths(),
            workspace.as_deref(),
            config.permissions.clone(),
            state.permissions().session_rules(&handle.session_id),
        ));
        let sink: Arc<dyn EventSink> = Arc::new(handle.clone());
        let prompter: Arc<dyn Prompter> = Arc::new(AgentPrompter::new(
            Arc::clone(state.permissions()),
            Arc::clone(&sink),
        ));
        let agent = handle.agent_id.to_string();

        conversation.configure(&config, opts);
        if conversation.system().is_empty() {
            conversation.set_system(build_system(workspace.as_ref(), &config).await);
        }
        let tools_changed = conversation
            .set_tools(state.tools().defs(&config.tools.disabled))
            .is_some();
        if tools_changed {
            warn!(
                new = %conversation.tools_hash().unwrap_or_default(),
                "tool set changed since the session started; the cached prefix is lost"
            );
            handle.emit(Event::AgentWarning {
                agent_id: agent.clone(),
                kind: "tools_changed".into(),
                message: "the tool set changed since the session started; \
                          the next call rebuilds the cache"
                    .into(),
            });
        }
        record_prefix(state, handle, &session, conversation).await?;
        Ok(Self {
            state,
            handle,
            hooks,
            config,
            mentor,
            workspace,
            engine,
            prompter,
            sink,
            mode,
            seen: Arc::new(SeenFiles::new()),
            agent,
            warned_context: false,
            continued: false,
        })
    }

    fn emit(&self, event: Event) {
        self.handle.emit(event);
    }

    /// Starts the watchdog: a task that ends the run (cancels it, with
    /// the stalled mark set) once no event has been emitted for
    /// [`stall_window`]. `None` when the window is zero.
    fn spawn_watchdog(&self) -> Option<tokio::task::JoinHandle<()>> {
        let window = stall_window(&self.config);
        if window.is_zero() {
            return None;
        }
        let handle = self.handle.clone();
        let activity = Arc::clone(handle.activity());
        activity.touch();
        Some(tokio::spawn(async move {
            let tick = (window / 4).clamp(Duration::from_millis(200), Duration::from_secs(15));
            loop {
                tokio::time::sleep(tick).await;
                if handle.cancel.is_cancelled() {
                    return;
                }
                let idle = activity.idle();
                if idle >= window {
                    warn!(
                        agent = %handle.agent_id,
                        idle_s = idle.as_secs(),
                        window_s = window.as_secs(),
                        "no sign of life; ending the run as stalled"
                    );
                    activity.mark_stalled();
                    handle.cancel.cancel();
                    return;
                }
            }
        }))
    }

    /// Records an outcome (at `at` when given, else on the agent) and
    /// tells the subscribers. A trace failure is logged, not fatal:
    /// the run goes on without the label.
    async fn record_outcome(&self, outcome: &Outcome, at: Option<&StepRef>) {
        let ev = match at {
            Some(at) => outcome.event_at(at),
            None => outcome.event(self.handle.session_id.clone(), self.handle.agent_id.clone()),
        };
        match self.state.writer().append(ev).await {
            Ok(id) => {
                let (summary, ok) = outcome.describe();
                info!(kind = outcome.kind, %summary, ?ok, "outcome recorded");
                self.emit(outcome.live_event(&self.handle.agent_id, id.as_str()));
            }
            Err(e) => warn!(error = %e, kind = outcome.kind, "cannot record the outcome"),
        }
    }

    /// The `reverted` outcome of the previous run, when `start` shows
    /// its changes undone (see [`super::revert`]).
    async fn check_reverted(&self, start: &Snapshot) {
        let Some(ws) = self.workspace.as_ref() else {
            return;
        };
        match super::revert::detect(
            self.state,
            &self.handle.session_id,
            &self.handle.agent_id,
            ws.root(),
            start,
        ) {
            Ok(Some((previous, outcome))) => {
                let ev = outcome.event(self.handle.session_id.clone(), previous.clone());
                match self.state.writer().append(ev).await {
                    Ok(id) => {
                        info!(agent = %previous, files = outcome.details["files"].as_array().map_or(0, Vec::len), "previous run's changes reverted");
                        self.emit(outcome.live_event(&previous, id.as_str()));
                    }
                    Err(e) => warn!(error = %e, "cannot record the reverted outcome"),
                }
            }
            Ok(None) => {}
            Err(e) => debug!(error = %e, "revert check skipped"),
        }
    }

    /// Takes and records a workspace snapshot; `None` without a
    /// workspace.
    async fn snapshot(&self, phase: SnapshotPhase) -> Option<Snapshot> {
        let ws = self.workspace.as_ref()?;
        let snap = ws.snapshot().await;
        let ev = snap.event(
            self.handle.session_id.clone(),
            Some(self.handle.agent_id.clone()),
            phase,
        );
        if let Err(e) = self.state.writer().append(ev).await {
            warn!(error = %e, ?phase, "cannot record workspace.snapshot");
        }
        Some(snap)
    }

    /// The end snapshot and the `files_changed` outcome against `start`.
    async fn finish_snapshot(&self, start: Option<Snapshot>) {
        let Some(start) = start else { return };
        let Some(end) = self.snapshot(SnapshotPhase::End).await else {
            return;
        };
        let changed = end.changes_since(&start);
        self.record_outcome(&changed.outcome(), None).await;
    }

    async fn record(&self, ev: NewEvent) -> Result<(), RpcError> {
        self.state.writer().append(ev).await?;
        Ok(())
    }

    /// The last message of `conv` as its store row.
    fn row(&self, conv: &Conversation, at: &StepRef) -> NewMessage {
        let (seq, msg) = conv.last_row().expect("a message was just pushed");
        NewMessage {
            session: self.handle.session_id.clone(),
            seq,
            role: msg.role,
            content: msg.content.clone(),
            agent: Some(at.agent.clone()),
            step: Some(at.step.clone()),
        }
    }

    /// `outcome {kind: error}` for a run the loop stops itself.
    async fn record_error_outcome(&self, kind: &str, details: Value) {
        self.record_outcome(&Outcome::error(kind, &details), None)
            .await;
    }

    async fn start_step(&self) -> Result<StepRef, RpcError> {
        let agent = self.handle.agent_id.clone();
        let step = self
            .state
            .writer()
            .run(move |store| store.start_step(&agent))
            .await?;
        Ok(StepRef {
            session: self.handle.session_id.clone(),
            agent: self.handle.agent_id.clone(),
            step,
        })
    }

    async fn finish_step(&self, at: &StepRef, status: RunStatus) -> Result<(), RpcError> {
        let step = at.step.clone();
        self.state
            .writer()
            .run(move |store| store.finish_step(&step, status))
            .await?;
        Ok(())
    }

    /// The loop. `Err` is a trace failure; everything else is a
    /// `Finish`.
    async fn run_loop(&mut self, conv: &mut Conversation) -> Result<Finish, RpcError> {
        let max = u64::from(self.config.runtime.max_iterations.max(1));
        for seq in 1..=max {
            if self.handle.cancel.is_cancelled() {
                return Ok(Finish::cancelled());
            }
            if let Some(stop) = self.guard_context(conv).await {
                return Ok(stop);
            }
            self.hooks
                .before_call(&mut CallContext {
                    conversation: conv,
                    step: seq,
                })
                .await;

            let at = self.start_step().await?;
            self.emit(Event::AgentStep {
                agent_id: self.agent.clone(),
                seq,
                phase: StepPhase::Mentor,
            });
            let call_id = CallId::generate();
            let resp = match self.call_mentor(conv, &at, &call_id).await? {
                Ok(resp) => resp,
                Err(err) => {
                    let status = if matches!(err, MentorError::Cancelled) {
                        RunStatus::Cancelled
                    } else {
                        RunStatus::Error
                    };
                    let step = at.step.clone();
                    let error = self
                        .state
                        .writer()
                        .run(move |store| {
                            store.record_mentor_error(&at, &call_id, &err, 0, true)?;
                            store.finish_step(&step, status)?;
                            Ok(RpcError::from(err))
                        })
                        .await?;
                    return Ok(Finish::from_error(error));
                }
            };
            self.after_response(conv, &at, &call_id, &resp).await?;

            let calls = tool_calls(&resp);
            match resp.stop_reason {
                StopReason::ToolUse => {
                    if self.run_tools(conv, &at, seq, calls).await? {
                        return Ok(Finish::cancelled());
                    }
                }
                StopReason::MaxTokens if !calls.is_empty() => {
                    // Cut off mid-way through tool calls: run what
                    // arrived (an incomplete input is an error result
                    // the mentor reads) rather than leave them dangling.
                    if self.run_tools(conv, &at, seq, calls).await? {
                        return Ok(Finish::cancelled());
                    }
                }
                StopReason::MaxTokens => {
                    self.finish_step(&at, RunStatus::Ok).await?;
                    if self.continued {
                        return Ok(Finish::ok(true));
                    }
                    self.continued = true;
                    conv.push_user_text(CONTINUE_MESSAGE);
                    let row = self.row(conv, &at);
                    let ev = at
                        .event(kinds::USER_MESSAGE)
                        .payload(json!({
                            "text_len": CONTINUE_MESSAGE.len(),
                            "synthetic": "continue",
                        }))
                        .blob_bytes(CONTINUE_MESSAGE, "text/plain; charset=utf-8");
                    self.state
                        .writer()
                        .run(move |store| store.append_with_message(ev, &row))
                        .await?;
                }
                StopReason::Refusal => {
                    self.finish_step(&at, RunStatus::Ok).await?;
                    let details = json!({
                        "category": resp.stop_details.as_ref().and_then(|d| d.category.clone()),
                        "explanation": resp.stop_details.as_ref().and_then(|d| d.explanation.clone()),
                    });
                    self.record_error_outcome("refusal", details.clone()).await;
                    let category = details["category"].as_str().unwrap_or("unspecified");
                    return Ok(Finish::stopped(
                        "refusal",
                        format!("the mentor refused to continue (category: {category})"),
                        details,
                    ));
                }
                StopReason::PauseTurn => {
                    // A server-side tool loop wants the same
                    // conversation back; the assistant turn is in it.
                    self.finish_step(&at, RunStatus::Ok).await?;
                }
                StopReason::ModelContextWindowExceeded => {
                    self.finish_step(&at, RunStatus::Ok).await?;
                    let details = json!({ "context_tokens": conv.context_tokens() });
                    self.record_error_outcome("context_limit", details.clone())
                        .await;
                    return Ok(Finish::stopped(
                        "context_limit",
                        "the conversation no longer fits the model's context window".to_owned(),
                        details,
                    ));
                }
                StopReason::EndTurn | StopReason::StopSequence | StopReason::Other(_) => {
                    self.finish_step(&at, RunStatus::Ok).await?;
                    return Ok(Finish::ok(false));
                }
            }
        }
        let details = json!({ "max_iterations": max });
        self.record_error_outcome("max_iterations", details.clone())
            .await;
        Ok(Finish::stopped(
            "max_iterations",
            format!("stopped after {max} mentor calls (runtime.max_iterations)"),
            details,
        ))
    }

    /// The context guards, before a call: past the hard limit the run
    /// stops; past the soft one it warns, once.
    async fn guard_context(&mut self, conv: &mut Conversation) -> Option<Finish> {
        let mut tokens = conv.context_tokens();
        // A resumed session with a history and no usage yet: ask the
        // API how big the prefix is before sending it.
        if tokens.is_none() && conv.message_count() > 1 {
            match self.mentor.count_tokens(&conv.request()).await {
                Ok(n) => tokens = Some(n),
                Err(e) => debug!(error = %e, "count_tokens failed; skipping the context guard"),
            }
        }
        let tokens = tokens?;
        let hard = self.config.mentor.context_hard_limit;
        let soft = self.config.mentor.context_soft_limit;
        if hard > 0 && tokens >= hard {
            let details = json!({ "context_tokens": tokens, "limit": hard });
            self.record_error_outcome("context_limit", details.clone())
                .await;
            return Some(Finish::stopped(
                "context_limit",
                format!("the context reached {tokens} tokens (mentor.context_hard_limit {hard})"),
                details,
            ));
        }
        if soft > 0 && tokens >= soft && !self.warned_context {
            self.warned_context = true;
            self.emit(Event::AgentWarning {
                agent_id: self.agent.clone(),
                kind: "context_large".into(),
                message: format!(
                    "the last call used {tokens} input tokens (mentor.context_soft_limit {soft})"
                ),
            });
        }
        None
    }

    /// One mentor call with the runtime's own retries on top of the
    /// adapter's: a rate limit or an overload waits (`retry-after`, else
    /// [`DEFAULT_WAIT`]) within `runtime.max_wait_s`; a stream that
    /// broke after content started is tried once more. The outer
    /// `Err` is a trace failure; the inner one the mentor's final word.
    async fn call_mentor(
        &self,
        conv: &Conversation,
        at: &StepRef,
        call_id: &CallId,
    ) -> Result<Result<MentorResponse, MentorError>, RpcError> {
        let req = conv.request();
        let body = match self.mentor.request_body(&req) {
            Ok(b) => b,
            Err(e) => return Ok(Err(e)),
        };
        {
            let (at, call_id, req) = (at.clone(), call_id.clone(), req.clone());
            let version = conv.prompt_version().map(str::to_owned);
            self.state
                .writer()
                .run(move |store| {
                    store.record_mentor_request(
                        &at,
                        &call_id,
                        CallKind::Step,
                        &req,
                        &body,
                        version.as_deref(),
                    )
                })
                .await?;
        }

        let max_wait = Duration::from_secs(self.config.runtime.max_wait_s);
        let mut waited = Duration::ZERO;
        let mut retry_no = 0;
        let mut interrupted = false;
        loop {
            let mut streaming = Streaming::new(self.handle, &self.agent);
            let result = self
                .mentor
                .complete(
                    &req,
                    &mut |ev| streaming.on_event(ev),
                    self.handle.cancel.clone(),
                )
                .await;
            let err = match result {
                Ok(resp) => return Ok(Ok(resp)),
                Err(e) => e,
            };
            let wait = match &err {
                MentorError::RateLimited { retry_after } => {
                    Some(retry_after.unwrap_or(DEFAULT_WAIT))
                }
                MentorError::Overloaded => Some(DEFAULT_WAIT),
                MentorError::StreamInterrupted { .. } if !interrupted => {
                    interrupted = true;
                    Some(Duration::ZERO)
                }
                _ => None,
            };
            let Some(wait) = wait else {
                return Ok(Err(err));
            };
            if waited + wait > max_wait {
                warn!(error = %err, waited_s = waited.as_secs(), "out of waiting budget");
                return Ok(Err(err));
            }
            retry_no += 1;
            // The non-final `mentor.error` (the call row stays open).
            let (http_status, api_kind) = err.http();
            self.record(at.event(kinds::MENTOR_ERROR).payload(json!({
                "call_id": call_id,
                "kind": err.kind(),
                "message": err.to_string(),
                "http_status": http_status,
                "api_error_type": api_kind,
                "retry_no": retry_no,
            })))
            .await?;
            if wait.is_zero() {
                self.emit(Event::AgentWarning {
                    agent_id: self.agent.clone(),
                    kind: "stream_interrupted".into(),
                    message: "the stream broke after output started; \
                              calling again (the partial output above is discarded)"
                        .into(),
                });
                continue;
            }
            let until = time::OffsetDateTime::now_utc() + wait;
            info!(
                reason = err.kind(),
                wait_s = wait.as_secs(),
                "waiting for the mentor"
            );
            self.emit(Event::AgentWaiting {
                agent_id: self.agent.clone(),
                reason: err.kind().to_owned(),
                until: format_ts(until),
                wait_ms: u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
            });
            tokio::select! {
                biased;
                () = self.handle.cancel.cancelled() => return Ok(Err(MentorError::Cancelled)),
                () = tokio::time::sleep(wait) => {}
            }
            waited += wait;
        }
    }

    /// Records the response, the assistant message (its event and its
    /// row) and the usage, and appends the assistant turn — every
    /// block, verbatim.
    async fn after_response(
        &self,
        conv: &mut Conversation,
        at: &StepRef,
        call_id: &CallId,
        resp: &MentorResponse,
    ) -> Result<(), RpcError> {
        let cost_micros = price_call(&resp.model, &resp.usage, &self.config.pricing);
        let text = resp.text();
        let tool_names: Vec<&str> = resp.tool_uses().iter().map(|(_, n, _)| *n).collect();
        let stop_reason = serde_json::to_value(&resp.stop_reason)
            .map_err(|e| RpcError::internal(e.to_string()))?;
        let message = at
            .event(kinds::ASSISTANT_MESSAGE)
            .payload(json!({
                "call_id": call_id,
                "text_len": text.len(),
                "stop_reason": stop_reason,
                "truncated": resp.stop_reason == StopReason::MaxTokens,
                "tool_calls": tool_names,
            }))
            .blob_bytes(text, "text/plain; charset=utf-8");
        conv.push_assistant(resp.content.clone());
        {
            let (at, call_id, resp) = (at.clone(), call_id.clone(), resp.clone());
            let row = self.row(conv, &at);
            self.state
                .writer()
                .run(move |store| {
                    store.record_mentor_response(&at, &call_id, &resp, cost_micros)?;
                    store.append_with_message(message, &row)
                })
                .await?;
        }
        conv.record_usage(resp.usage, cost_micros);
        self.emit(Event::AgentUsage {
            agent_id: self.agent.clone(),
            call_id: call_id.to_string(),
            usage: resp.usage,
            cost_usd: cost_micros.map(micros_to_usd),
            session_usage: conv.totals(),
            session_cost_usd: conv.cost_micros().map(micros_to_usd),
            session_calls: conv.calls(),
        });
        Ok(())
    }

    /// Runs the calls of a step under the permission gate, reports
    /// each as it ends, appends the results as one user message and
    /// finishes the step. Returns whether the agent was cancelled
    /// meanwhile (the results are in the history either way).
    async fn run_tools(
        &self,
        conv: &mut Conversation,
        at: &StepRef,
        seq: u64,
        mut calls: Vec<ToolCall>,
    ) -> Result<bool, RpcError> {
        self.emit(Event::AgentStep {
            agent_id: self.agent.clone(),
            seq,
            phase: StepPhase::Tools,
        });
        self.hooks
            .before_tool_exec(&mut ToolExecContext {
                calls: &mut calls,
                step: seq,
            })
            .await;
        // What the outcome heuristics need of each call once it ran.
        let inputs: HashMap<String, (String, Value)> = calls
            .iter()
            .map(|c| (c.id.clone(), (c.name.clone(), c.input.clone())))
            .collect();

        let (progress_tx, mut progress_rx) = mpsc::channel::<ToolProgress>(64);
        let forwarder = {
            let handle = self.handle.clone();
            let agent = self.agent.clone();
            tokio::spawn(async move {
                while let Some(p) = progress_rx.recv().await {
                    handle.emit(Event::AgentToolProgress {
                        agent_id: agent.clone(),
                        call_id: p.call_id,
                        stream: p.stream.into(),
                        text: p.text,
                    });
                }
            })
        };
        let observer = {
            let handle = self.handle.clone();
            let agent = self.agent.clone();
            Box::new(move |done: &Executed| {
                handle.emit(Event::AgentToolResult {
                    agent_id: agent.clone(),
                    call_id: done.call_id.clone(),
                    name: done.name.clone(),
                    ok: done.ok,
                    summary: done.summary.clone(),
                    blob_id: done.blob_id.as_ref().map(ToString::to_string),
                    mentor_bytes: done.mentor_bytes as u64,
                    event_id: done.result_event.as_ref().map(ToString::to_string),
                });
            })
        };
        let gate = PermissionGate::new(
            Arc::clone(&self.engine),
            Arc::clone(&self.prompter),
            self.state.writer(),
            at.clone(),
            self.mode,
        )
        .with_sink(Arc::clone(&self.sink));
        let mut results = {
            let executor = Executor::new(
                self.state.tools(),
                &gate,
                self.state.writer(),
                &self.config.tools,
                at.clone(),
                self.handle.cancel.clone(),
            )
            .with_workspace(self.workspace.clone())
            .with_seen_files(Arc::clone(&self.seen))
            .with_progress(progress_tx)
            .with_observer(observer);
            executor.execute_all(calls).await
        };
        // The executor (and its sender) is gone: the forwarder drains
        // what is left and ends.
        let _ = forwarder.await;

        self.hooks
            .on_tool_result(&mut ToolResultContext {
                results: &mut results,
                step: seq,
            })
            .await;
        for r in &results {
            if r.kind != ToolResultKind::Ok && r.kind != ToolResultKind::Error {
                continue;
            }
            let Some((name, input)) = inputs.get(&r.call_id) else {
                continue;
            };
            let text = result_text(&r.block);
            if let Some(outcome) = outcomes::from_tool_result(name, input, text, &r.metadata) {
                self.record_outcome(&outcome, Some(at)).await;
            }
        }
        let cancelled = results.iter().any(|r| r.kind == ToolResultKind::Cancelled)
            || self.handle.cancel.is_cancelled();
        conv.push_tool_results(results.into_iter().map(|r| r.block).collect());
        let row = self.row(conv, at);
        self.state
            .writer()
            .run(move |store| store.put_session_message(&row))
            .await?;
        self.finish_step(
            at,
            if cancelled {
                RunStatus::Cancelled
            } else {
                RunStatus::Ok
            },
        )
        .await?;
        Ok(cancelled)
    }
}

/// The text of a `tool_result` block (what the mentor reads).
fn result_text(block: &ContentBlock) -> &str {
    match block {
        ContentBlock::ToolResult { content, .. } => content
            .iter()
            .find_map(|c| match c {
                ToolResultContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .unwrap_or(""),
        _ => "",
    }
}

/// The `tool_use` blocks of a response as calls, in order.
fn tool_calls(resp: &MentorResponse) -> Vec<ToolCall> {
    resp.tool_uses()
        .into_iter()
        .map(|(id, name, input)| ToolCall::new(id, name, input.clone()))
        .collect()
}

/// Forwards stream events as they arrive and announces each tool call
/// once its block (and so its input) is complete.
struct Streaming<'a> {
    handle: &'a AgentHandle,
    agent: &'a str,
    /// Tool blocks in flight by index: `(id, name, input JSON so far)`.
    tools: HashMap<usize, (String, String, String)>,
}

impl<'a> Streaming<'a> {
    fn new(handle: &'a AgentHandle, agent: &'a str) -> Self {
        Self {
            handle,
            agent,
            tools: HashMap::new(),
        }
    }

    fn on_event(&mut self, ev: StreamEvent) {
        match ev {
            StreamEvent::TextDelta(text) => self.handle.emit(Event::AgentTextDelta {
                agent_id: self.agent.to_owned(),
                text,
            }),
            StreamEvent::ThinkingDelta(text) => self.handle.emit(Event::AgentThinkingDelta {
                agent_id: self.agent.to_owned(),
                text,
            }),
            StreamEvent::ToolUseStart { index, id, name } => {
                self.tools.insert(index, (id, name, String::new()));
            }
            StreamEvent::ToolInputDelta {
                index,
                partial_json,
            } => {
                if let Some((_, _, json)) = self.tools.get_mut(&index) {
                    json.push_str(&partial_json);
                }
            }
            StreamEvent::BlockStop(index) => {
                if let Some((id, name, json)) = self.tools.remove(&index) {
                    let input = if json.trim().is_empty() {
                        Value::Object(serde_json::Map::new())
                    } else {
                        serde_json::from_str(&json).unwrap_or(Value::String(json))
                    };
                    self.handle.emit(Event::AgentToolCall {
                        agent_id: self.agent.to_owned(),
                        call_id: id,
                        name,
                        input,
                    });
                }
            }
            StreamEvent::Usage(_) | StreamEvent::Done => {}
        }
    }
}
