//! JSON-RPC 2.0 message types and the harness error model.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Request identifier. Numbers are used by our clients; strings are accepted
/// for interoperability.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(u64),
    String(String),
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Id::Number(n) => write!(f, "{n}"),
            Id::String(s) => write!(f, "{s}"),
        }
    }
}

/// A request from a client to the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: Version,
    pub id: Id,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// A notification (no id, no response expected). Used for daemon → client
/// events and for client → daemon fire-and-forget messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: Version,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// A response to a request: exactly one of `result` / `error` is present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: Version,
    /// `None` only for errors that could not be associated with a request
    /// (parse errors); serialised as `null` per the spec.
    pub id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn success(id: Id, result: Value) -> Self {
        Self {
            jsonrpc: Version,
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(id: Option<Id>, error: RpcError) -> Self {
        Self {
            jsonrpc: Version,
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// The literal `"2.0"` version tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Version;

impl Serialize for Version {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = String::deserialize(d)?;
        if v == "2.0" {
            Ok(Version)
        } else {
            Err(serde::de::Error::custom(format!(
                "unsupported jsonrpc version {v:?}"
            )))
        }
    }
}

/// Any message on the wire. Parsed by shape: `method` + `id` → request,
/// `method` without `id` → notification, otherwise a response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    Request(Request),
    Notification(Notification),
    Response(Response),
}

impl Message {
    pub fn request(id: Id, method: impl Into<String>, params: Option<Value>) -> Self {
        Message::Request(Request {
            jsonrpc: Version,
            id,
            method: method.into(),
            params,
        })
    }

    pub fn notification(method: impl Into<String>, params: Option<Value>) -> Self {
        Message::Notification(Notification {
            jsonrpc: Version,
            method: method.into(),
            params,
        })
    }
}

/// Error payload. `data.kind` is a stable machine-readable discriminator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<ErrorData>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorData {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.data {
            Some(d) => write!(f, "{} [{}] ({})", self.message, d.kind, self.code),
            None => write!(f, "{} ({})", self.message, self.code),
        }
    }
}

impl std::error::Error for RpcError {}

/// Error codes. Reserved JSON-RPC codes first, then application codes.
pub mod codes {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;

    pub const UNAUTHORIZED: i32 = -32001;
    pub const INCOMPATIBLE_API: i32 = -32002;
    pub const NOT_FOUND: i32 = -32010;
    pub const CONFLICT: i32 = -32011;
    pub const MENTOR_ERROR: i32 = -32020;
    pub const MENTOR_RATE_LIMITED: i32 = -32021;
    pub const CANCELLED: i32 = -32030;
    /// The agent stopped short of `end_turn`: `data.kind` is `refusal`,
    /// `max_iterations` or `context_limit`.
    pub const AGENT_STOPPED: i32 = -32031;
    pub const PERMISSION_DENIED: i32 = -32040;
    pub const CONFIG_ERROR: i32 = -32050;
}

impl RpcError {
    pub fn new(code: i32, kind: &str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(ErrorData {
                kind: kind.to_owned(),
                details: None,
            }),
        }
    }

    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        match &mut self.data {
            Some(d) => d.details = Some(details),
            None => {
                self.data = Some(ErrorData {
                    kind: String::new(),
                    details: Some(details),
                });
            }
        }
        self
    }

    /// The `data.kind` discriminator, if present.
    pub fn kind(&self) -> Option<&str> {
        self.data.as_ref().map(|d| d.kind.as_str())
    }

    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::new(codes::PARSE_ERROR, "parse_error", message)
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_REQUEST, "invalid_request", message)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            codes::METHOD_NOT_FOUND,
            "method_not_found",
            format!("unknown method {method}"),
        )
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_PARAMS, "invalid_params", message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(codes::INTERNAL_ERROR, "internal", message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(codes::UNAUTHORIZED, "unauthorized", message)
    }

    pub fn incompatible_api(client: u32, daemon: u32) -> Self {
        Self::new(
            codes::INCOMPATIBLE_API,
            "incompatible_api",
            format!("client speaks API v{client}, daemon speaks API v{daemon}"),
        )
        .with_details(serde_json::json!({ "client_api": client, "daemon_api": daemon }))
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(codes::NOT_FOUND, "not_found", message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(codes::CONFLICT, "conflict", message)
    }

    pub fn cancelled() -> Self {
        Self::new(codes::CANCELLED, "cancelled", "operation cancelled")
    }

    /// The agent ended because of `kind` (`refusal`, `max_iterations`,
    /// `context_limit`) rather than an error of the mentor or the
    /// daemon.
    pub fn agent_stopped(kind: &str, message: impl Into<String>) -> Self {
        Self::new(codes::AGENT_STOPPED, kind, message)
    }

    pub fn permission_denied(message: impl Into<String>) -> Self {
        Self::new(codes::PERMISSION_DENIED, "permission_denied", message)
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::new(codes::CONFIG_ERROR, "config_error", message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_shapes_are_distinguished() {
        let req: Message =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"daemon.status"}"#).unwrap();
        assert!(matches!(req, Message::Request(_)));

        let note: Message =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"event","params":{}}"#).unwrap();
        assert!(matches!(note, Message::Notification(_)));

        let resp: Message =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#).unwrap();
        assert!(matches!(resp, Message::Response(_)));

        let err: Message = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"bad"}}"#,
        )
        .unwrap();
        match err {
            Message::Response(r) => {
                assert_eq!(r.id, None);
                assert_eq!(r.error.unwrap().code, codes::PARSE_ERROR);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn wrong_version_is_rejected() {
        let r: Result<Request, _> =
            serde_json::from_str(r#"{"jsonrpc":"1.0","id":1,"method":"x"}"#);
        assert!(r.is_err());
    }

    #[test]
    fn error_round_trip_and_display() {
        let e = RpcError::incompatible_api(1, 2);
        let json = serde_json::to_string(&e).unwrap();
        let back: RpcError = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
        assert_eq!(back.kind(), Some("incompatible_api"));
        assert!(back.to_string().contains("[incompatible_api]"));
    }

    #[test]
    fn response_serialises_null_id_for_parse_errors() {
        let r = Response::failure(None, RpcError::parse_error("x"));
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["id"], Value::Null);
        assert!(json.get("result").is_none());
    }
}
