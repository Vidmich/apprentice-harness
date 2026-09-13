//! Request and response types of the Messages API, serialised exactly as
//! the wire expects. Nothing here knows about sessions or traces.

use std::collections::BTreeMap;

pub use apprentice_api::types::{Effort, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::config::ThinkingDisplay;

/// One complete request. `model`, `max_tokens` and `effort` come from
/// config; the runtime fills the rest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MentorRequest {
    pub model: String,
    pub max_tokens: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub system: Vec<SystemBlock>,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDef>,
    #[serde(default)]
    pub thinking: Thinking,
    #[serde(default = "default_effort")]
    pub effort: Effort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
}

fn default_effort() -> Effort {
    Effort::High
}

/// The body of `POST /v1/messages` as sent: a [`MentorRequest`] in the
/// API's shape, serialised deterministically (fixed field order, no
/// whitespace) by [`request_body`]. The trace stores these bytes as the
/// `mentor.request` blob; `replay-check` (task M01-14) parses them back
/// and expects [`request_body`] to return the same bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireRequest {
    pub model: String,
    pub max_tokens: u32,
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub system: Vec<SystemBlock>,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDef>,
    #[serde(default)]
    pub thinking: Thinking,
    pub output_config: OutputConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
}

/// `output_config` of the wire body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputConfig {
    pub effort: Effort,
}

impl From<&MentorRequest> for WireRequest {
    fn from(req: &MentorRequest) -> Self {
        Self {
            model: req.model.clone(),
            max_tokens: req.max_tokens,
            stream: true,
            system: req.system.clone(),
            messages: req.messages.clone(),
            tools: req.tools.clone(),
            thinking: req.thinking,
            output_config: OutputConfig { effort: req.effort },
            metadata: req.metadata.clone(),
        }
    }
}

impl WireRequest {
    /// The request the body was built from.
    pub fn into_request(self) -> MentorRequest {
        MentorRequest {
            model: self.model,
            max_tokens: self.max_tokens,
            system: self.system,
            messages: self.messages,
            tools: self.tools,
            thinking: self.thinking,
            effort: self.output_config.effort,
            metadata: self.metadata,
        }
    }
}

/// The exact bytes the mentor sends for `req` (see [`WireRequest`]).
///
/// # Errors
/// Serialisation failure (does not happen for well-formed types).
pub fn request_body(req: &MentorRequest) -> serde_json::Result<Vec<u8>> {
    serde_json::to_vec(&WireRequest::from(req))
}

/// A system prompt block. `cache` renders as a cache breakpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "text")]
pub struct SystemBlock {
    pub text: String,
    #[serde(flatten)]
    pub cache: CacheFlag,
}

impl SystemBlock {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            cache: CacheFlag(false),
        }
    }

    #[must_use]
    pub fn cached(mut self) -> Self {
        self.cache = CacheFlag(true);
        self
    }
}

/// Message role. `System` is the mid-conversation system message the
/// current models accept inside `messages` (used by the compactor).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
}

/// A conversation message. Content is always serialised in block form;
/// the string form is accepted on input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(deserialize_with = "string_or_blocks")]
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::text(text)],
        }
    }

    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Self {
            role: Role::Assistant,
            content,
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: vec![ContentBlock::text(text)],
        }
    }
}

/// Renders as `"cache_control": {"type": "ephemeral"}` when set; absent
/// otherwise. Flattened into the blocks that support breakpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheFlag(pub bool);

impl Serialize for CacheFlag {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(usize::from(self.0)))?;
        if self.0 {
            m.serialize_entry("cache_control", &serde_json::json!({"type": "ephemeral"}))?;
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for CacheFlag {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Helper {
            #[serde(default)]
            cache_control: Option<Value>,
        }
        Ok(Self(Helper::deserialize(d)?.cache_control.is_some()))
    }
}

/// A content block in either direction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
        #[serde(flatten)]
        cache: CacheFlag,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        #[serde(flatten)]
        cache: CacheFlag,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default, deserialize_with = "string_or_result_blocks")]
        content: Vec<ToolResultContent>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
        #[serde(flatten)]
        cache: CacheFlag,
    },
    /// Echo back unchanged (including `signature`) on later turns.
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    Image {
        source: Value,
    },
    Document {
        source: Value,
    },
    /// Anything newer than this crate; passed through untouched.
    #[serde(untagged)]
    Unknown(Value),
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            cache: CacheFlag(false),
        }
    }

    pub fn tool_result(
        tool_use_id: impl Into<String>,
        text: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: vec![ToolResultContent::Text { text: text.into() }],
            is_error,
            cache: CacheFlag(false),
        }
    }

    /// Marks this block as a cache breakpoint (no-op for thinking blocks).
    #[must_use]
    pub fn cached(mut self) -> Self {
        match &mut self {
            Self::Text { cache, .. }
            | Self::ToolUse { cache, .. }
            | Self::ToolResult { cache, .. } => {
                *cache = CacheFlag(true);
            }
            _ => {}
        }
        self
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text, .. } => Some(text),
            _ => None,
        }
    }
}

/// Content inside a `tool_result`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContent {
    Text {
        text: String,
    },
    Image {
        source: Value,
    },
    #[serde(untagged)]
    Unknown(Value),
}

/// A tool the mentor may call. Serialised in the order given.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(flatten)]
    pub cache: CacheFlag,
}

/// Thinking configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Thinking {
    Adaptive {
        display: ThinkingDisplay,
    },
    /// Only valid with effort `high` or lower.
    Disabled,
}

impl Default for Thinking {
    fn default() -> Self {
        Self::Adaptive {
            display: ThinkingDisplay::Summarized,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

/// Why the model stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    Refusal,
    PauseTurn,
    ModelContextWindowExceeded,
    #[serde(untagged)]
    Other(String),
}

/// Details attached to some stop reasons (notably `refusal`). Branch on
/// `StopReason`, never on this — it is informational and may be absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct StopDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Wall-clock timing of one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Timing {
    pub first_byte_ms: u64,
    pub total_ms: u64,
    /// Attempts made (1 = no retry).
    pub attempts: u32,
}

/// A fully assembled response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MentorResponse {
    pub id: String,
    pub model: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_details: Option<StopDetails>,
    pub usage: Usage,
    pub timing: Timing,
    /// Bytes of the serialised request body.
    pub request_bytes: usize,
    /// Raw SSE bytes when `trace.capture_raw_sse` is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_sse: Option<Vec<u8>>,
}

impl MentorResponse {
    /// Concatenated text blocks.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(ContentBlock::as_text)
            .collect()
    }

    /// `(id, name, input)` of every tool call, in order.
    pub fn tool_uses(&self) -> Vec<(&str, &str, &Value)> {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => Some((id.as_str(), name.as_str(), input)),
                _ => None,
            })
            .collect()
    }
}

/// Incremental events delivered while a response streams.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolUseStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolInputDelta {
        index: usize,
        partial_json: String,
    },
    BlockStop(usize),
    /// Cumulative usage so far (input fields from `message_start`, output
    /// from `message_delta`).
    Usage(Usage),
    Done,
}

// -------------------------------------------------------------- helpers

fn string_or_blocks<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<ContentBlock>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Text(String),
        Blocks(Vec<ContentBlock>),
    }
    Ok(match Either::deserialize(d)? {
        Either::Text(t) => vec![ContentBlock::text(t)],
        Either::Blocks(b) => b,
    })
}

fn string_or_result_blocks<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<ToolResultContent>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Text(String),
        Blocks(Vec<ToolResultContent>),
    }
    Ok(match Either::deserialize(d)? {
        Either::Text(text) => vec![ToolResultContent::Text { text }],
        Either::Blocks(b) => b,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cache_flag_renders_only_when_set() {
        let plain = serde_json::to_value(ContentBlock::text("hi")).unwrap();
        assert_eq!(plain, json!({"type": "text", "text": "hi"}));
        let cached = serde_json::to_value(ContentBlock::text("hi").cached()).unwrap();
        assert_eq!(
            cached,
            json!({"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}})
        );
        let back: ContentBlock = serde_json::from_value(cached).unwrap();
        assert_eq!(back, ContentBlock::text("hi").cached());
    }

    #[test]
    fn system_block_shape() {
        let b = serde_json::to_value(SystemBlock::new("You are").cached()).unwrap();
        assert_eq!(
            b,
            json!({"type": "text", "text": "You are", "cache_control": {"type": "ephemeral"}})
        );
    }

    #[test]
    fn message_accepts_string_content() {
        let m: Message =
            serde_json::from_value(json!({"role": "user", "content": "hello"})).unwrap();
        assert_eq!(m, Message::user("hello"));
        let m: Message = serde_json::from_value(
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]}),
        )
        .unwrap();
        assert_eq!(
            m.content,
            vec![ContentBlock::tool_result("t1", "ok", false)]
        );
    }

    #[test]
    fn thinking_blocks_round_trip_unchanged() {
        let v = json!({"type": "thinking", "thinking": "hmm", "signature": "sig=="});
        let b: ContentBlock = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(serde_json::to_value(&b).unwrap(), v);
        let v = json!({"type": "redacted_thinking", "data": "xx"});
        let b: ContentBlock = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(serde_json::to_value(&b).unwrap(), v);
    }

    #[test]
    fn unknown_block_passes_through() {
        let v = json!({"type": "server_tool_use", "id": "x", "name": "web_search", "input": {}});
        let b: ContentBlock = serde_json::from_value(v.clone()).unwrap();
        assert!(matches!(b, ContentBlock::Unknown(_)));
        assert_eq!(serde_json::to_value(&b).unwrap(), v);
    }

    #[test]
    fn stop_reason_unknown_is_kept() {
        let r: StopReason = serde_json::from_value(json!("something_new")).unwrap();
        assert_eq!(r, StopReason::Other("something_new".into()));
        assert_eq!(
            serde_json::from_value::<StopReason>(json!("tool_use")).unwrap(),
            StopReason::ToolUse
        );
    }

    #[test]
    fn thinking_default_is_adaptive_summarized() {
        assert_eq!(
            serde_json::to_value(Thinking::default()).unwrap(),
            json!({"type": "adaptive", "display": "summarized"})
        );
        assert_eq!(
            serde_json::to_value(Thinking::Disabled).unwrap(),
            json!({"type": "disabled"})
        );
    }
}
