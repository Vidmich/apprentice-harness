//! The [`Gate`] the executor calls: builds the request, lets the
//! [`Engine`] decide, records `permission.decision`.

use std::sync::Arc;

use apprentice_api::events::Event;
use apprentice_api::types::{PermissionDecision, PermissionMode};
use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::warn;

use super::{Engine, EventSink, Outcome, PermissionRequest, Prompter};
use crate::tools::{Gate, ToolContext, ToolSpec};
use crate::trace::{StepRef, TraceWriter, kinds};

/// One per step, like the executor it feeds.
pub struct PermissionGate<'a> {
    engine: Arc<Engine>,
    prompter: Arc<dyn Prompter>,
    writer: &'a TraceWriter,
    at: StepRef,
    mode: PermissionMode,
    sink: Option<Arc<dyn EventSink>>,
}

impl std::fmt::Debug for PermissionGate<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionGate")
            .field("at", &self.at)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl<'a> PermissionGate<'a> {
    pub fn new(
        engine: Arc<Engine>,
        prompter: Arc<dyn Prompter>,
        writer: &'a TraceWriter,
        at: StepRef,
        mode: PermissionMode,
    ) -> Self {
        Self {
            engine,
            prompter,
            writer,
            at,
            mode,
            sink: None,
        }
    }

    /// Where the live `permission.decision` events go.
    #[must_use]
    pub fn with_sink(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    pub fn mode(&self) -> PermissionMode {
        self.mode
    }

    /// The `permission.decision` payload.
    fn payload(&self, call_id: &str, req: &PermissionRequest, outcome: &Outcome) -> Value {
        let mut payload = json!({
            "call_id": call_id,
            "tool": req.tool,
            "risk": req.risk,
            "mode": self.mode,
            "decision": outcome.decision,
            "source": outcome.source,
            "asked": outcome.request_id.is_some(),
            "waited_ms": u64::try_from(outcome.waited.as_millis()).unwrap_or(u64::MAX),
            "paths": req.shown_paths(),
            "outside_workspace": req.has_outside(),
        });
        if let Some(c) = &req.command {
            payload["command"] = json!(c);
        }
        if let Some(r) = &outcome.rule_ref {
            payload["rule_ref"] = json!(r);
        }
        if let Some(r) = &outcome.reason {
            payload["reason"] = json!(r);
        }
        if let Some(id) = &outcome.request_id {
            payload["request_id"] = json!(id);
        }
        if let Some(a) = outcome.answer {
            payload["answer"] = json!(a);
        }
        if let Some(w) = &outcome.rule_written {
            payload["rule_written"] = json!({
                "path": w.path.to_string_lossy(),
                "line": w.line,
                "rule": w.rule,
            });
        }
        if !outcome.notes.is_empty() {
            payload["notes"] = json!(outcome.notes);
        }
        payload
    }
}

#[async_trait]
impl Gate for PermissionGate<'_> {
    async fn permit(
        &self,
        ctx: &ToolContext,
        spec: &ToolSpec,
        input: &Value,
    ) -> Result<(), String> {
        let req = PermissionRequest::for_call(spec, input, ctx.workspace.as_deref());
        let outcome = self
            .engine
            .decide(&req, &ctx.agent_id, self.mode, &*self.prompter)
            .await;
        let payload = self.payload(&ctx.call_id, &req, &outcome);
        if let Err(e) = self
            .writer
            .append(self.at.event(kinds::PERMISSION_DECISION).payload(payload))
            .await
        {
            warn!(call = %ctx.call_id, error = %e, "cannot record permission.decision");
        }
        if let Some(sink) = &self.sink {
            sink.emit(Event::PermissionDecision {
                agent_id: ctx.agent_id.to_string(),
                call_id: ctx.call_id.clone(),
                tool: req.tool.clone(),
                decision: outcome.decision,
                source: outcome.source,
                request_id: outcome.request_id.clone(),
                rule_ref: outcome.rule_ref.clone(),
                reason: outcome.reason.clone(),
            });
        }
        match outcome.decision {
            PermissionDecision::Allow => Ok(()),
            PermissionDecision::Deny | _ => {
                Err(outcome.reason.unwrap_or_else(|| "denied".to_owned()))
            }
        }
    }
}
