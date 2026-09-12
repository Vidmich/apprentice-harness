//! Higher-level recording of mentor calls: builds the documented
//! `mentor.request` / `mentor.response` / `mentor.error` payloads and keeps
//! the `mentor_calls` row in step, each as one transaction. The runtime
//! (task M00-11) calls these around `Mentor::complete`.

use serde_json::json;

use super::blobs::sha256_hex;
use super::error::TraceError;
use super::store::{MentorCallEnd, MentorCallStart, NewEvent, TraceStore};
use super::{AgentId, CallId, EventId, RunStatus, SessionId, StepId, kinds};
use crate::mentor::{MentorError, MentorRequest, MentorResponse};

/// Where a mentor call happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRef {
    pub session: SessionId,
    pub agent: AgentId,
    pub step: StepId,
}

impl StepRef {
    /// A new event of `kind` at this step.
    pub fn event(&self, kind: &str) -> NewEvent {
        NewEvent::new(self.session.clone(), kind)
            .agent(self.agent.clone())
            .step(self.step.clone())
    }
}

impl TraceStore {
    /// Records the request exactly as it will be sent (`body` is the
    /// replay unit and is always stored as a blob) and opens the
    /// `mentor_calls` row.
    pub fn record_mentor_request(
        &self,
        at: &StepRef,
        call_id: &CallId,
        req: &MentorRequest,
        body: &[u8],
    ) -> Result<EventId, TraceError> {
        let effort = serde_json::to_value(req.effort)?
            .as_str()
            .map(str::to_owned);
        let system_hash = if req.system.is_empty() {
            None
        } else {
            Some(sha256_hex(&serde_json::to_vec(&req.system)?))
        };
        let payload = json!({
            "call_id": call_id,
            "model": req.model,
            "effort": effort,
            "max_tokens": req.max_tokens,
            "message_count": req.messages.len(),
            "tool_names": req.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            "system_hash": system_hash,
            "request_hash": sha256_hex(body),
            "bytes": body.len(),
        });
        let ev = at
            .event(kinds::MENTOR_REQUEST)
            .payload(payload)
            .blob_bytes(body, "application/json");
        let call = MentorCallStart {
            id: call_id.clone(),
            session: at.session.clone(),
            agent: at.agent.clone(),
            step: at.step.clone(),
            request_event: EventId::from(""), // overwritten by `append_with_call`
            model: req.model.clone(),
            effort,
            request_bytes: Some(body.len() as u64),
            started_at: None,
        };
        self.append_with_call(ev, &call)
    }

    /// Records the assembled response (content blocks as the blob, raw SSE
    /// as a second blob when captured) and completes the call with
    /// `cost_micros` from token accounting (`None` = unpriced model).
    pub fn record_mentor_response(
        &self,
        at: &StepRef,
        call_id: &CallId,
        resp: &MentorResponse,
        cost_micros: Option<i64>,
    ) -> Result<EventId, TraceError> {
        let raw_sse_blob = match &resp.raw_sse {
            Some(raw) => Some(self.put_blob(raw, "text/event-stream")?),
            None => None,
        };
        let stop_reason = serde_json::to_value(&resp.stop_reason)?;
        let mut payload = json!({
            "call_id": call_id,
            "response_id": resp.id,
            "model": resp.model,
            "stop_reason": stop_reason,
            "usage": resp.usage,
            "first_byte_ms": resp.timing.first_byte_ms,
            "total_ms": resp.timing.total_ms,
            "attempts": resp.timing.attempts,
        });
        if let Some(d) = &resp.stop_details {
            payload["stop_details"] = serde_json::to_value(d)?;
        }
        if let Some(id) = &raw_sse_blob {
            payload["raw_sse_blob_id"] = json!({ super::payload::BLOB_REF_KEY: id });
        }
        let content = serde_json::to_vec(&resp.content)?;
        let ev = at
            .event(kinds::MENTOR_RESPONSE)
            .payload(payload)
            .blob_bytes(content, "application/json");
        let end = MentorCallEnd {
            id: call_id.clone(),
            response_event: None, // linked to the appended event by the store
            status: RunStatus::Ok,
            stop_reason: stop_reason.as_str().map(str::to_owned),
            http_status: Some(200),
            usage: Some(resp.usage),
            cost_micros,
            first_byte_ms: Some(resp.timing.first_byte_ms),
            total_ms: Some(resp.timing.total_ms),
        };
        let ids = self.append_with_completion(vec![ev], &end, true)?;
        Ok(ids.into_iter().next().expect("one event appended"))
    }

    /// Records a failed attempt. When `final_attempt` is set the call is
    /// completed as `error` (or `cancelled` for [`MentorError::Cancelled`]).
    pub fn record_mentor_error(
        &self,
        at: &StepRef,
        call_id: &CallId,
        err: &MentorError,
        retry_no: u32,
        final_attempt: bool,
    ) -> Result<EventId, TraceError> {
        let (http_status, api_kind) = err.http();
        let payload = json!({
            "call_id": call_id,
            "kind": err.kind(),
            "message": err.to_string(),
            "http_status": http_status,
            "api_error_type": api_kind,
            "retry_no": retry_no,
        });
        let ev = at.event(kinds::MENTOR_ERROR).payload(payload);
        if !final_attempt {
            return self.append(ev);
        }
        let status = if matches!(err, MentorError::Cancelled) {
            RunStatus::Cancelled
        } else {
            RunStatus::Error
        };
        let end = MentorCallEnd {
            http_status,
            ..MentorCallEnd::new(call_id.clone(), status)
        };
        let ids = self.append_with_completion(vec![ev], &end, false)?;
        Ok(ids.into_iter().next().expect("one event appended"))
    }
}
