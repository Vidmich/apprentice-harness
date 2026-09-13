//! The execution wrapper around one tool call, and the policy for a
//! batch of them.
//!
//! One call: look the tool up, validate the input, record `tool.call`,
//! ask the [`Gate`], run under the timeout and the cancellation token,
//! capture the raw output as the `tool.result` blob, cut the mentor's
//! copy to its budget, record `tool.result`, return the `tool_result`
//! block. Nothing here aborts the agent: every failure becomes an error
//! result the mentor can react to, and every call — however it ended —
//! leaves its two events in the trace.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::limits::{capture_bytes, one_line, truncate_utf8};
use super::registry::Entry;
use super::{
    MAX_SUMMARY_CHARS, Risk, SeenFiles, ToolContent, ToolContext, ToolEnv, ToolError, ToolOutput,
    ToolProgress, ToolRegistry, ToolSpec,
};
use crate::config::ToolsConfig;
use crate::mentor::ContentBlock;
use crate::trace::{BlobId, EventId, StepRef, TraceError, TraceWriter, kinds, sha256_hex};
use crate::workspace::Workspace;

/// A `tool_use` block as the mentor sent it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The `tool_use` id; echoed as `tool_use_id`.
    pub id: String,
    pub name: String,
    pub input: Value,
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, input: Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            input,
        }
    }
}

/// How a call ended; `tool.result.kind` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultKind {
    /// Ran and succeeded.
    Ok,
    /// Ran and reported `is_error` (the mentor sees the diagnostic).
    Error,
    /// Schema violation, or a tool that is not registered.
    InvalidInput,
    Denied,
    Timeout,
    Cancelled,
    /// Could not run (I/O or internal failure) or the trace refused it.
    Failed,
}

impl ToolResultKind {
    fn of(err: &ToolError) -> Self {
        match err {
            ToolError::InvalidInput(_) => Self::InvalidInput,
            ToolError::Denied(_) => Self::Denied,
            ToolError::Timeout => Self::Timeout,
            ToolError::Cancelled => Self::Cancelled,
            ToolError::Io(_) | ToolError::Failed(_) => Self::Failed,
        }
    }
}

/// One executed call: the block for the next mentor message plus what
/// the events and the trace need to know about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Executed {
    pub call_id: String,
    pub name: String,
    /// The `tool_result` block, in the mentor's budget.
    pub block: ContentBlock,
    pub kind: ToolResultKind,
    /// `kind == Ok`.
    pub ok: bool,
    pub summary: String,
    /// The raw output, when there was one.
    pub blob_id: Option<BlobId>,
    /// Length of the raw output (before any capture cut).
    pub output_bytes: usize,
    /// Length of the text the mentor receives.
    pub mentor_bytes: usize,
    /// The mentor's copy is shorter than the output.
    pub truncated: bool,
    pub duration: Duration,
    pub result_event: Option<EventId>,
    /// The tool's structured facts (`tool.result.metadata`): exit code
    /// and timing of a shell run, the parsed outcome of `run_tests`.
    pub metadata: Value,
}

impl Executed {
    /// `true` when the agent was cancelled during this call.
    pub fn is_cancelled(&self) -> bool {
        self.kind == ToolResultKind::Cancelled
    }
}

/// The permission hook: [`crate::permissions::PermissionGate`] in the
/// runtime, [`AllowAll`] where nothing needs asking.
#[async_trait]
pub trait Gate: Send + Sync {
    /// `Err(reason)` refuses the call; `reason` is what the mentor reads
    /// ("denied by user").
    async fn permit(&self, ctx: &ToolContext, spec: &ToolSpec, input: &Value)
    -> Result<(), String>;
}

/// Permits everything.
#[derive(Debug, Clone, Copy, Default)]
pub struct AllowAll;

#[async_trait]
impl Gate for AllowAll {
    async fn permit(&self, _: &ToolContext, _: &ToolSpec, _: &Value) -> Result<(), String> {
        Ok(())
    }
}

/// Called with every call the moment it is done (results of a batch
/// come back together; this is how the runtime reports them live).
pub type Observer = dyn Fn(&Executed) + Send + Sync;

/// Executes the calls of one step. Built per step by the runtime.
pub struct Executor<'a> {
    registry: &'a ToolRegistry,
    gate: &'a dyn Gate,
    writer: &'a TraceWriter,
    config: Arc<ToolsConfig>,
    at: StepRef,
    cancel: CancellationToken,
    workspace: Option<Arc<Workspace>>,
    seen: Arc<SeenFiles>,
    progress: mpsc::Sender<ToolProgress>,
    observer: Option<Box<Observer>>,
}

impl std::fmt::Debug for Executor<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Executor")
            .field("at", &self.at)
            .field("tools", &self.registry.len())
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

impl<'a> Executor<'a> {
    /// An executor for the calls of step `at`, cancelled by `cancel`
    /// (the agent's token). Progress goes nowhere until
    /// [`Self::with_progress`].
    pub fn new(
        registry: &'a ToolRegistry,
        gate: &'a dyn Gate,
        writer: &'a TraceWriter,
        config: &'a ToolsConfig,
        at: StepRef,
        cancel: CancellationToken,
    ) -> Self {
        let (progress, _) = mpsc::channel(1);
        Self {
            registry,
            gate,
            writer,
            config: Arc::new(config.clone()),
            at,
            cancel,
            workspace: None,
            seen: Arc::new(SeenFiles::new()),
            progress,
            observer: None,
        }
    }

    #[must_use]
    pub fn with_workspace(mut self, workspace: Option<Arc<Workspace>>) -> Self {
        self.workspace = workspace;
        self
    }

    /// The agent's seen-files set (one per agent, across its steps; a
    /// fresh one per executor otherwise).
    #[must_use]
    pub fn with_seen_files(mut self, seen: Arc<SeenFiles>) -> Self {
        self.seen = seen;
        self
    }

    #[must_use]
    pub fn with_progress(mut self, progress: mpsc::Sender<ToolProgress>) -> Self {
        self.progress = progress;
        self
    }

    /// Told of every executed call as it finishes.
    #[must_use]
    pub fn with_observer(mut self, observer: Box<Observer>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Executes every call of one assistant message: calls that cannot
    /// change anything (read-only, network, unknown names) run
    /// concurrently, then `Write`/`Execute` calls run one after another
    /// in the order given. Results come back in the original order.
    pub async fn execute_all(&self, calls: Vec<ToolCall>) -> Vec<Executed> {
        let mutating: Vec<bool> = calls
            .iter()
            .map(|c| {
                self.registry
                    .get(&c.name)
                    .is_some_and(|e| e.spec.is_mutating())
            })
            .collect();
        let mut slots: Vec<Option<Executed>> = (0..calls.len()).map(|_| None).collect();
        let (concurrent, sequential): (Vec<_>, Vec<_>) = calls
            .into_iter()
            .enumerate()
            .partition(|(i, _)| !mutating[*i]);

        let done = join_all(
            concurrent
                .into_iter()
                .map(|(i, call)| async move { (i, self.execute_one(call).await) }),
        )
        .await;
        for (i, executed) in done {
            slots[i] = Some(executed);
        }
        for (i, call) in sequential {
            slots[i] = Some(self.execute_one(call).await);
        }
        slots
            .into_iter()
            .map(|s| s.expect("every call executed"))
            .collect()
    }

    /// Executes one call end to end (see the module docs).
    pub async fn execute_one(&self, call: ToolCall) -> Executed {
        let entry = self.registry.get(&call.name);
        let spec = entry.as_ref().map(|e| e.spec.clone());
        let env = spec
            .as_ref()
            .map_or_else(ToolEnv::default, |s| ToolEnv::resolve(&self.config, s));
        let ctx = ToolContext {
            workspace: self.workspace.clone(),
            session_id: self.at.session.clone(),
            agent_id: self.at.agent.clone(),
            call_id: call.id.clone(),
            env,
            config: Arc::clone(&self.config),
            seen: Arc::clone(&self.seen),
            progress: self.progress.clone(),
        };

        // Recorded whatever happens next, so the trace has the mentor's
        // request even when it was for a tool that does not exist.
        if let Err(e) = self.record_call(&call, spec.as_ref().map(|s| s.risk)).await {
            warn!(call = %call.id, error = %e, "cannot record tool.call");
            let err = ToolError::Failed(format!("trace: {e}"));
            return self
                .finish(&call, &ctx, Err(err), Duration::ZERO, None)
                .await;
        }

        let started = Instant::now();
        let outcome = match &entry {
            None => Err(unknown_tool(&call.name, self.registry)),
            Some(entry) => self.run(entry, &ctx, &call).await,
        };
        let duration = started.elapsed();
        let executed = self
            .finish(&call, &ctx, outcome, duration, spec.as_ref())
            .await;
        if let Some(observer) = &self.observer {
            observer(&executed);
        }
        executed
    }

    async fn run(
        &self,
        entry: &Entry,
        ctx: &ToolContext,
        call: &ToolCall,
    ) -> Result<ToolOutput, ToolError> {
        entry.validator.validate(&call.input)?;
        // The gate may wait on a person; cancellation ends the wait.
        let permit = self.gate.permit(ctx, &entry.spec, &call.input);
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => return Err(ToolError::Cancelled),
            r = permit => r.map_err(ToolError::Denied)?,
        }
        let token = self.cancel.child_token();
        let fut = entry.tool.call(ctx, call.input.clone(), token.clone());
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => {
                token.cancel();
                Err(ToolError::Cancelled)
            }
            r = tokio::time::timeout(ctx.env.timeout, fut) => r.unwrap_or_else(|_| {
                token.cancel();
                Err(ToolError::Timeout)
            }),
        }
    }

    async fn record_call(
        &self,
        call: &ToolCall,
        risk: Option<Risk>,
    ) -> Result<EventId, TraceError> {
        let input = serde_json::to_vec(&call.input)?;
        let payload = json!({
            "call_id": call.id,
            "name": call.name,
            "risk": risk,
            "input_hash": sha256_hex(&input),
            "input_bytes": input.len(),
        });
        self.writer
            .append(
                self.at
                    .event(kinds::TOOL_CALL)
                    .payload(payload)
                    .blob_bytes(input, "application/json"),
            )
            .await
    }

    /// Turns the outcome into the mentor's block, records `tool.result`.
    async fn finish(
        &self,
        call: &ToolCall,
        ctx: &ToolContext,
        outcome: Result<ToolOutput, ToolError>,
        duration: Duration,
        spec: Option<&ToolSpec>,
    ) -> Executed {
        let name = call.name.as_str();
        let (kind, text, summary, raw, metadata, message, attachments) = match outcome {
            Ok(output) => {
                let (raw, media_type) = output.content.to_bytes();
                let kind = if output.is_error {
                    ToolResultKind::Error
                } else {
                    ToolResultKind::Ok
                };
                let summary = if output.summary.trim().is_empty() {
                    default_summary(name, kind, raw.len(), duration)
                } else {
                    output.summary
                };
                (
                    kind,
                    None::<String>,
                    summary,
                    Some((raw, media_type.to_owned(), output.content)),
                    output.metadata,
                    None,
                    output.attachments,
                )
            }
            Err(err) => {
                let kind = ToolResultKind::of(&err);
                let text = match &err {
                    ToolError::InvalidInput(m) => format!("invalid input for {name}: {m}"),
                    ToolError::Denied(reason) => reason.clone(),
                    ToolError::Timeout => {
                        format!("{name} timed out after {} s", ctx.env.timeout.as_secs())
                    }
                    ToolError::Cancelled => "cancelled".to_owned(),
                    ToolError::Io(e) => format!("{name} failed: {e}"),
                    ToolError::Failed(m) => format!("{name} failed: {m}"),
                };
                let summary = default_summary_err(name, &err, ctx.env.timeout);
                (
                    kind,
                    Some(text),
                    summary,
                    None,
                    Value::Null,
                    Some(err.to_string()),
                    Vec::new(),
                )
            }
        };
        let summary = one_line(&summary, MAX_SUMMARY_CHARS);

        // The raw blob first: the mentor's marker names it.
        let mut output_bytes = 0;
        let mut truncated_at_capture = false;
        let mut blob_id = None;
        let mut media_type = None;
        let mut content = None;
        if let Some((raw, mt, c)) = raw {
            output_bytes = raw.len();
            let stored = match capture_bytes(&raw, ctx.env.max_capture_bytes) {
                Some(cut) => {
                    truncated_at_capture = true;
                    cut
                }
                None => raw,
            };
            let mt_for_store = mt.clone();
            match self
                .writer
                .run(move |store| store.put_blob(&stored, &mt_for_store))
                .await
            {
                Ok(id) => blob_id = Some(id),
                Err(e) => warn!(call = %call.id, error = %e, "cannot store tool output"),
            }
            media_type = Some(mt);
            content = Some(c);
        }
        // Side outputs: a blob each (the tool keeps them within bounds),
        // listed by name in the payload.
        let mut attached = Vec::with_capacity(attachments.len());
        for a in attachments {
            let bytes = a.bytes.len();
            let (data, mt) = (a.bytes, a.media_type.clone());
            match self
                .writer
                .run(move |store| store.put_blob(&data, &mt))
                .await
            {
                Ok(id) => attached.push(json!({
                    "name": a.name,
                    "blob_id": id,
                    "bytes": bytes,
                    "media_type": a.media_type,
                })),
                Err(e) => {
                    warn!(call = %call.id, name = %a.name, error = %e, "cannot store attachment");
                }
            }
        }

        let (text, truncated) = match (text, content) {
            (Some(t), _) => (t, false),
            (None, Some(c)) => mentor_text(&c, ctx.env.max_mentor_bytes, blob_id.as_ref()),
            (None, None) => (String::new(), false),
        };
        let is_error = kind != ToolResultKind::Ok;
        let mentor_bytes = text.len();
        let block = ContentBlock::tool_result(call.id.clone(), text, is_error);

        let mut payload = json!({
            "call_id": call.id,
            "name": name,
            "ok": !is_error,
            "kind": kind,
            "duration_ms": u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            "output_bytes": output_bytes,
            "mentor_bytes": mentor_bytes,
            "truncated": truncated,
            "summary": summary,
        });
        if truncated_at_capture {
            payload["truncated_at_capture"] = json!(true);
        }
        if let Some(mt) = &media_type {
            payload["media_type"] = json!(mt);
        }
        if let Some(m) = &message {
            payload["message"] = json!(m);
        }
        if !metadata.is_null() {
            payload["metadata"] = metadata.clone();
        }
        if !attached.is_empty() {
            payload["attachments"] = Value::Array(attached);
        }
        if let Some(risk) = spec.map(|s| s.risk) {
            payload["risk"] = json!(risk);
        }
        let mut ev = self.at.event(kinds::TOOL_RESULT).payload(payload);
        if let Some(id) = &blob_id {
            ev = ev.blob(id.clone());
        }
        let result_event = match self.writer.append(ev).await {
            Ok(id) => Some(id),
            Err(e) => {
                warn!(call = %call.id, error = %e, "cannot record tool.result");
                None
            }
        };
        debug!(call = %call.id, tool = name, ?kind, ms = duration.as_millis(), "tool call ended");
        Executed {
            call_id: call.id.clone(),
            name: name.to_owned(),
            block,
            kind,
            ok: !is_error,
            summary,
            blob_id,
            output_bytes,
            mentor_bytes,
            truncated,
            duration,
            result_event,
            metadata,
        }
    }
}

fn unknown_tool(name: &str, registry: &ToolRegistry) -> ToolError {
    let names = registry.names();
    if names.is_empty() {
        ToolError::InvalidInput(format!("unknown tool {name:?}; no tools are available"))
    } else {
        ToolError::InvalidInput(format!(
            "unknown tool {name:?}; available: {}",
            names.join(", ")
        ))
    }
}

/// What the mentor reads for a successful output, within `max_bytes`,
/// and whether it was cut.
fn mentor_text(content: &ToolContent, max_bytes: usize, blob: Option<&BlobId>) -> (String, bool) {
    let text = match content {
        ToolContent::Text(t) => t.clone(),
        ToolContent::Json(v) => serde_json::to_string_pretty(v).unwrap_or_default(),
        ToolContent::Binary { media_type, bytes } => {
            let text = match blob {
                Some(id) => format!(
                    "[binary result: {media_type}, {} bytes, full result id {id}]",
                    bytes.len()
                ),
                None => format!("[binary result: {media_type}, {} bytes]", bytes.len()),
            };
            return (text, false);
        }
    };
    if text.is_empty() {
        return ("(no output)".to_owned(), false);
    }
    let marker = |omitted: usize| match blob {
        Some(id) => format!("[... {omitted} bytes omitted, full result id {id}]"),
        None => format!("[... {omitted} bytes omitted]"),
    };
    let cut = truncate_utf8(&text, max_bytes, marker);
    (cut.text, cut.truncated)
}

fn default_summary(name: &str, kind: ToolResultKind, bytes: usize, duration: Duration) -> String {
    let secs = duration.as_secs_f64();
    let time = if secs >= 1.0 {
        format!(" in {secs:.1} s")
    } else {
        String::new()
    };
    match kind {
        ToolResultKind::Error => format!("{name}: error, {bytes} bytes{time}"),
        _ => format!("{name}: {bytes} bytes{time}"),
    }
}

fn default_summary_err(name: &str, err: &ToolError, timeout: Duration) -> String {
    match err {
        ToolError::InvalidInput(_) => format!("{name}: invalid input"),
        ToolError::Denied(_) => format!("{name}: denied"),
        ToolError::Timeout => format!("{name}: timed out after {} s", timeout.as_secs()),
        ToolError::Cancelled => format!("{name}: cancelled"),
        ToolError::Io(e) => format!("{name}: failed: {e}"),
        ToolError::Failed(m) => format!("{name}: failed: {m}"),
    }
}
