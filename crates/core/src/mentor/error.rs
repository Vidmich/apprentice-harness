//! Mentor errors and their RPC mapping (-32020 `mentor_error`,
//! -32021 `mentor_rate_limited`, -32030 `cancelled`).

use std::time::Duration;

use apprentice_api::jsonrpc::{RpcError, codes};

use super::types::ContentBlock;

#[derive(Debug, thiserror::Error)]
pub enum MentorError {
    #[error("authentication failed (check the API key)")]
    Auth,

    #[error("rate limited{}", retry_after.map(|d| format!(" (retry after {}s)", d.as_secs())).unwrap_or_default())]
    RateLimited { retry_after: Option<Duration> },

    #[error("API overloaded")]
    Overloaded,

    #[error("invalid request: {message}")]
    InvalidRequest { message: String },

    #[error("request too large")]
    RequestTooLarge,

    #[error("API error {status} {kind}: {message}")]
    Api {
        status: u16,
        kind: String,
        message: String,
    },

    /// An `error` event arrived inside the SSE stream.
    #[error("stream error {kind}: {message}")]
    Stream { kind: String, message: String },

    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    /// The connection closed before any content arrived (retryable).
    #[error("connection closed before the response started")]
    Disconnected,

    #[error("protocol error: {0}")]
    Protocol(String),

    /// The connection dropped after content had started; not retried.
    #[error("stream interrupted after {} content block(s)", partial.len())]
    StreamInterrupted { partial: Vec<ContentBlock> },

    #[error("cancelled")]
    Cancelled,

    #[error("timed out")]
    Timeout,
}

impl MentorError {
    /// Whether a fresh attempt may succeed (used before any content has
    /// been received).
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimited { .. } | Self::Overloaded | Self::Timeout | Self::Disconnected => {
                true
            }
            Self::Api { status, .. } => *status >= 500,
            Self::Stream { kind, .. } => kind == "overloaded_error" || kind == "api_error",
            Self::Network(e) => !e.is_builder() && !e.is_redirect() && !e.is_decode(),
            _ => false,
        }
    }

    /// Stable machine-readable kind.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::RateLimited { .. } => "rate_limited",
            Self::Overloaded => "overloaded",
            Self::InvalidRequest { .. } => "invalid_request",
            Self::RequestTooLarge => "request_too_large",
            Self::Api { .. } => "api",
            Self::Stream { .. } => "stream",
            Self::Network(_) => "network",
            Self::Disconnected => "disconnected",
            Self::Protocol(_) => "protocol",
            Self::StreamInterrupted { .. } => "stream_interrupted",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
        }
    }

    /// `(http_status, api_error_type)` where known.
    pub fn http(&self) -> (Option<u16>, Option<&str>) {
        match self {
            Self::Auth => (Some(401), Some("authentication_error")),
            Self::RateLimited { .. } => (Some(429), Some("rate_limit_error")),
            Self::Overloaded => (Some(529), Some("overloaded_error")),
            Self::InvalidRequest { .. } => (Some(400), Some("invalid_request_error")),
            Self::RequestTooLarge => (Some(413), Some("request_too_large")),
            Self::Api { status, kind, .. } => (Some(*status), Some(kind)),
            Self::Stream { kind, .. } => (None, Some(kind)),
            _ => (None, None),
        }
    }
}

impl From<MentorError> for RpcError {
    fn from(e: MentorError) -> Self {
        match &e {
            MentorError::Cancelled => RpcError::cancelled(),
            MentorError::RateLimited { retry_after } => RpcError::new(
                codes::MENTOR_RATE_LIMITED,
                "mentor_rate_limited",
                e.to_string(),
            )
            .with_details(serde_json::json!({
                "retry_after_ms": retry_after.map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
            })),
            _ => {
                let (status, kind) = e.http();
                RpcError::new(codes::MENTOR_ERROR, "mentor_error", e.to_string()).with_details(
                    serde_json::json!({
                        "reason": e.kind(),
                        "http_status": status,
                        "api_error_type": kind,
                    }),
                )
            }
        }
    }
}
