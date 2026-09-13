//! Daemon → client notifications.
//!
//! All events travel in a JSON-RPC notification with method [`EVENT_METHOD`]
//! and params [`EventNotification`]. Clients must ignore event types they do
//! not know.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::jsonrpc::RpcError;
use crate::types::{PermissionDecision, PermissionSource, RuleSpec, Usage};

/// Notification method name carrying every event.
pub const EVENT_METHOD: &str = "event";

/// Params of an `event` notification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventNotification {
    /// Subscription the event belongs to (an agent id for `agent.run`).
    pub subscription: String,
    /// Monotonic per subscription, per connection; starts at 1.
    pub seq: u64,
    pub event: Event,
}

/// Terminal status of an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentStatus {
    Ok,
    Cancelled,
    Error,
}

/// Log level for `log` events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// Tool risk class (defined here so events can carry it before M01).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Risk {
    ReadOnly,
    Write,
    Execute,
    Network,
}

/// Every event the daemon can emit. Tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Event {
    #[serde(rename = "agent.started")]
    AgentStarted {
        agent_id: String,
        session_id: String,
    },
    #[serde(rename = "agent.text_delta")]
    AgentTextDelta { agent_id: String, text: String },
    #[serde(rename = "agent.thinking_delta")]
    AgentThinkingDelta { agent_id: String, text: String },
    #[serde(rename = "agent.tool_call")]
    AgentToolCall {
        agent_id: String,
        call_id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "agent.tool_result")]
    AgentToolResult {
        agent_id: String,
        call_id: String,
        ok: bool,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blob_id: Option<String>,
    },
    #[serde(rename = "agent.usage")]
    AgentUsage {
        agent_id: String,
        call_id: String,
        usage: Usage,
        /// Cost of this call from the daemon's pricing table; absent when
        /// the model has no entry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
    },
    #[serde(rename = "agent.finished")]
    AgentFinished {
        agent_id: String,
        status: AgentStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<RpcError>,
        /// The model stopped at `max_tokens`: the answer is incomplete
        /// although the run finished `ok`.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    /// The engine needs an answer: reply with `permission.respond`
    /// before `timeout_s` run out (task M01-07).
    #[serde(rename = "permission.request")]
    PermissionRequest {
        request_id: String,
        agent_id: String,
        tool: String,
        /// The input as it will run, long strings other than paths and
        /// commands cut short.
        input: Value,
        risk: Risk,
        /// One line for the prompt: the tool's own `description` when
        /// the call has one, else the command or the paths.
        #[serde(default)]
        description: String,
        /// The command that would run, whole (shell tools).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        /// The paths the call names, root-relative (absolute when
        /// outside the workspace).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        paths: Vec<String>,
        /// Rules an `allow_workspace` / `allow_always` / `deny_always`
        /// answer would write, most specific first; the answer may
        /// carry an edited one instead.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        suggested_rules: Vec<RuleSpec>,
        /// Seconds until the request is denied as timed out.
        #[serde(default)]
        timeout_s: u64,
    },
    /// Every permission decision, asked or not; also lets a client
    /// close a prompt another client answered.
    #[serde(rename = "permission.decision")]
    PermissionDecision {
        agent_id: String,
        call_id: String,
        tool: String,
        decision: PermissionDecision,
        source: PermissionSource,
        /// The `request_id` of the prompt this answers, when one was
        /// shown.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        /// `workspace:2`, `user:1`, `builtin:<name>`, `session:1`,
        /// `default:<source>`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rule_ref: Option<String>,
        /// Why, when denied.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    #[serde(rename = "log")]
    Log { level: LogLevel, message: String },
    /// Any event type this build does not know. Kept so clients can skip
    /// newer events without failing to parse the notification.
    #[serde(untagged)]
    Unknown(Value),
}

impl Event {
    /// `true` when this event ends its subscription.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Event::AgentFinished { .. })
    }

    /// Log-level warning event, used by clients when they drop events.
    pub fn warn(message: impl Into<String>) -> Self {
        Event::Log {
            level: LogLevel::Warn,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_event_types_are_preserved_not_rejected() {
        let n: EventNotification = serde_json::from_str(
            r#"{"subscription":"a","seq":1,"event":{"type":"agent.future_thing","x":1}}"#,
        )
        .unwrap();
        match n.event {
            Event::Unknown(v) => assert_eq!(v["type"], "agent.future_thing"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn known_event_round_trips() {
        let e = Event::AgentFinished {
            agent_id: "ag".into(),
            status: AgentStatus::Error,
            error: Some(RpcError::cancelled()),
            truncated: false,
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "agent.finished");
        let back: Event = serde_json::from_value(v).unwrap();
        assert_eq!(e, back);
        assert!(back.is_terminal());
    }
}
