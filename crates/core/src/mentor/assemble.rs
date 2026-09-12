//! Turns the Messages API stream events into [`StreamEvent`]s and a final
//! assembled response.

use serde::Deserialize;
use serde_json::Value;

use super::error::MentorError;
use super::types::{CacheFlag, ContentBlock, StopDetails, StopReason, StreamEvent, Usage};

/// One block being received.
#[derive(Debug, Clone)]
enum Partial {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        json: String,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking(String),
    Other(Value),
}

/// Stream state for one response.
#[derive(Debug, Default)]
pub struct Assembler {
    pub id: String,
    pub model: String,
    blocks: Vec<Option<Partial>>,
    done: Vec<Option<ContentBlock>>,
    pub usage: Usage,
    pub stop_reason: Option<StopReason>,
    pub stop_details: Option<StopDetails>,
    pub finished: bool,
    started: bool,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    message: Option<MessageStart>,
    #[serde(default)]
    content_block: Option<Value>,
    #[serde(default)]
    delta: Option<Value>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    error: Option<ApiErrorBody>,
}

#[derive(Deserialize)]
struct MessageStart {
    #[serde(default)]
    id: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    usage: Usage,
}

#[derive(Deserialize)]
pub(super) struct ApiErrorBody {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub message: String,
}

impl Assembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether any content block has started (decides retry eligibility).
    pub fn content_started(&self) -> bool {
        self.started
    }

    /// Feeds one SSE `data` payload. Returns the events to forward.
    ///
    /// # Errors
    /// Malformed JSON or block structure → `Protocol`; an API `error` event
    /// → `Stream`.
    pub fn feed(&mut self, data: &str) -> Result<Vec<StreamEvent>, MentorError> {
        let env: Envelope = serde_json::from_str(data)
            .map_err(|e| MentorError::Protocol(format!("bad event JSON: {e}")))?;
        let mut out = Vec::new();
        match env.kind.as_str() {
            "message_start" => {
                let m = env
                    .message
                    .ok_or_else(|| MentorError::Protocol("message_start without message".into()))?;
                self.id = m.id;
                self.model = m.model;
                self.usage = m.usage;
                out.push(StreamEvent::Usage(self.usage));
            }
            "content_block_start" => {
                let index = env.index.ok_or_else(|| {
                    MentorError::Protocol("content_block_start without index".into())
                })?;
                let block = env.content_block.unwrap_or(Value::Null);
                self.started = true;
                let partial = match block.get("type").and_then(Value::as_str) {
                    Some("text") => Partial::Text(
                        block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    ),
                    Some("tool_use") => {
                        let id = str_field(&block, "id");
                        let name = str_field(&block, "name");
                        out.push(StreamEvent::ToolUseStart {
                            index,
                            id: id.clone(),
                            name: name.clone(),
                        });
                        Partial::ToolUse {
                            id,
                            name,
                            json: String::new(),
                        }
                    }
                    Some("thinking") => Partial::Thinking {
                        thinking: str_field(&block, "thinking"),
                        signature: str_field(&block, "signature"),
                    },
                    Some("redacted_thinking") => {
                        Partial::RedactedThinking(str_field(&block, "data"))
                    }
                    _ => Partial::Other(block),
                };
                self.set_partial(index, partial);
            }
            "content_block_delta" => {
                let index = env.index.ok_or_else(|| {
                    MentorError::Protocol("content_block_delta without index".into())
                })?;
                let delta = env.delta.unwrap_or(Value::Null);
                let kind = delta.get("type").and_then(Value::as_str).unwrap_or("");
                let partial = self.partial_mut(index)?;
                match (kind, partial) {
                    ("text_delta", Partial::Text(t)) => {
                        let s = str_field(&delta, "text");
                        t.push_str(&s);
                        out.push(StreamEvent::TextDelta(s));
                    }
                    ("input_json_delta", Partial::ToolUse { json, .. }) => {
                        let s = str_field(&delta, "partial_json");
                        json.push_str(&s);
                        out.push(StreamEvent::ToolInputDelta {
                            index,
                            partial_json: s,
                        });
                    }
                    ("thinking_delta", Partial::Thinking { thinking, .. }) => {
                        let s = str_field(&delta, "thinking");
                        thinking.push_str(&s);
                        out.push(StreamEvent::ThinkingDelta(s));
                    }
                    ("signature_delta", Partial::Thinking { signature, .. }) => {
                        signature.push_str(&str_field(&delta, "signature"));
                    }
                    (other, _) => {
                        tracing::debug!(index, delta = other, "ignoring unknown delta");
                    }
                }
            }
            "content_block_stop" => {
                let index = env.index.ok_or_else(|| {
                    MentorError::Protocol("content_block_stop without index".into())
                })?;
                let partial = self
                    .blocks
                    .get_mut(index)
                    .and_then(Option::take)
                    .ok_or_else(|| {
                        MentorError::Protocol(format!("stop for unknown block {index}"))
                    })?;
                let block = finish(partial)?;
                if self.done.len() <= index {
                    self.done.resize(index + 1, None);
                }
                self.done[index] = Some(block);
                out.push(StreamEvent::BlockStop(index));
            }
            "message_delta" => {
                if let Some(delta) = &env.delta {
                    if let Some(r) = delta.get("stop_reason").filter(|v| !v.is_null()) {
                        self.stop_reason = serde_json::from_value(r.clone()).ok();
                    }
                    if let Some(d) = delta.get("stop_details").filter(|v| !v.is_null()) {
                        self.stop_details = serde_json::from_value(d.clone()).ok();
                    }
                }
                if let Some(u) = env.usage {
                    // Output tokens are cumulative; input/cache fields may be
                    // repeated here — keep the larger value of each.
                    self.usage.output_tokens = self.usage.output_tokens.max(u.output_tokens);
                    self.usage.input_tokens = self.usage.input_tokens.max(u.input_tokens);
                    self.usage.cache_read_input_tokens = self
                        .usage
                        .cache_read_input_tokens
                        .max(u.cache_read_input_tokens);
                    self.usage.cache_creation_input_tokens = self
                        .usage
                        .cache_creation_input_tokens
                        .max(u.cache_creation_input_tokens);
                    out.push(StreamEvent::Usage(self.usage));
                }
            }
            "message_stop" => {
                self.finished = true;
                out.push(StreamEvent::Done);
            }
            "ping" => {}
            "error" => {
                let e = env.error.unwrap_or(ApiErrorBody {
                    kind: "unknown".into(),
                    message: data.to_owned(),
                });
                return Err(MentorError::Stream {
                    kind: e.kind,
                    message: e.message,
                });
            }
            other => {
                tracing::debug!(event = other, "ignoring unknown stream event");
            }
        }
        Ok(out)
    }

    /// Completed blocks in index order (partial ones are finished as-is).
    pub fn content(&self) -> Vec<ContentBlock> {
        let mut out: Vec<ContentBlock> = self.done.iter().flatten().cloned().collect();
        for p in self.blocks.iter().flatten() {
            if let Ok(b) = finish(p.clone()) {
                out.push(b);
            }
        }
        out
    }

    /// Final stop reason (`end_turn` when the stream never said).
    pub fn stop_reason(&self) -> StopReason {
        self.stop_reason.clone().unwrap_or(StopReason::EndTurn)
    }

    fn set_partial(&mut self, index: usize, p: Partial) {
        if self.blocks.len() <= index {
            self.blocks.resize(index + 1, None);
        }
        self.blocks[index] = Some(p);
    }

    fn partial_mut(&mut self, index: usize) -> Result<&mut Partial, MentorError> {
        self.blocks
            .get_mut(index)
            .and_then(Option::as_mut)
            .ok_or_else(|| MentorError::Protocol(format!("delta for unknown block {index}")))
    }
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn finish(p: Partial) -> Result<ContentBlock, MentorError> {
    Ok(match p {
        Partial::Text(text) => ContentBlock::Text {
            text,
            cache: CacheFlag(false),
        },
        Partial::ToolUse { id, name, json } => {
            let input = if json.trim().is_empty() {
                Value::Object(serde_json::Map::new())
            } else {
                serde_json::from_str(&json).map_err(|e| {
                    MentorError::Protocol(format!("tool_use {name} input is not JSON: {e}"))
                })?
            };
            ContentBlock::ToolUse {
                id,
                name,
                input,
                cache: CacheFlag(false),
            }
        }
        Partial::Thinking {
            thinking,
            signature,
        } => ContentBlock::Thinking {
            thinking,
            signature,
        },
        Partial::RedactedThinking(data) => ContentBlock::RedactedThinking { data },
        Partial::Other(v) => serde_json::from_value(v.clone()).unwrap_or(ContentBlock::Unknown(v)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembles_text_and_tool_use_with_split_json() {
        let mut a = Assembler::new();
        let events = [
            r#"{"type":"message_start","message":{"id":"msg_1","model":"m","usage":{"input_tokens":10,"cache_read_input_tokens":4}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu_1","name":"read","input":{}}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"pa"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"th\":\"a.rs\"}"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":7}}"#,
            r#"{"type":"message_stop"}"#,
        ];
        let mut got = Vec::new();
        for e in events {
            got.extend(a.feed(e).unwrap());
        }
        assert!(a.finished);
        assert_eq!(a.id, "msg_1");
        assert_eq!(a.stop_reason(), StopReason::ToolUse);
        assert_eq!(a.usage.input_tokens, 10);
        assert_eq!(a.usage.cache_read_input_tokens, 4);
        assert_eq!(a.usage.output_tokens, 7);
        let content = a.content();
        assert_eq!(content[0].as_text(), Some("Hello"));
        assert!(matches!(
            &content[1],
            ContentBlock::ToolUse { id, name, input, .. }
                if id == "tu_1" && name == "read" && input["path"] == "a.rs"
        ));
        assert!(got.contains(&StreamEvent::TextDelta("Hel".into())));
        assert!(got.contains(&StreamEvent::ToolUseStart {
            index: 1,
            id: "tu_1".into(),
            name: "read".into()
        }));
        assert_eq!(got.last(), Some(&StreamEvent::Done));
    }

    #[test]
    fn empty_tool_input_becomes_empty_object() {
        let mut a = Assembler::new();
        a.feed(r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t","name":"ls","input":{}}}"#).unwrap();
        a.feed(r#"{"type":"content_block_stop","index":0}"#)
            .unwrap();
        assert!(
            matches!(&a.content()[0], ContentBlock::ToolUse { input, .. } if input.as_object().unwrap().is_empty())
        );
    }

    #[test]
    fn bad_tool_json_and_error_event() {
        let mut a = Assembler::new();
        a.feed(r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t","name":"ls","input":{}}}"#).unwrap();
        a.feed(r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{oops"}}"#).unwrap();
        assert!(matches!(
            a.feed(r#"{"type":"content_block_stop","index":0}"#),
            Err(MentorError::Protocol(_))
        ));
        let mut a = Assembler::new();
        assert!(matches!(
            a.feed(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#),
            Err(MentorError::Stream { kind, .. }) if kind == "overloaded_error"
        ));
    }

    #[test]
    fn thinking_and_refusal_details() {
        let mut a = Assembler::new();
        a.feed(r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#).unwrap();
        a.feed(r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me"}}"#).unwrap();
        a.feed(r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc"}}"#).unwrap();
        a.feed(r#"{"type":"content_block_stop","index":0}"#)
            .unwrap();
        a.feed(r#"{"type":"message_delta","delta":{"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber","explanation":null}},"usage":{"output_tokens":3}}"#).unwrap();
        assert_eq!(
            a.content()[0],
            ContentBlock::Thinking {
                thinking: "Let me".into(),
                signature: "abc".into()
            }
        );
        assert_eq!(a.stop_reason(), StopReason::Refusal);
        let d = a.stop_details.clone().unwrap();
        assert_eq!(d.category.as_deref(), Some("cyber"));
        assert_eq!(d.extra["type"], "refusal");
    }
}
