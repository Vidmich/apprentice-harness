//! `AnthropicMentor`: the Messages API over raw HTTPS with streaming,
//! retries and cancellation.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::StatusCode;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::assemble::{ApiErrorBody, Assembler};
use super::error::MentorError;
use super::sse::SseParser;
use super::types::{
    Effort, MentorRequest, MentorResponse, Message, Metadata, SystemBlock, Thinking, Timing,
    ToolDef,
};
use super::{EventSink, Mentor};
use crate::config::MentorConfig;
use crate::secrets::Secret;

/// API version header sent with every request.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Longest single backoff between attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Messages API over HTTPS.
#[derive(Debug, Clone)]
pub struct AnthropicMentor {
    model: String,
    base_url: String,
    max_retries: u32,
    key: Secret,
    http: reqwest::Client,
    capture_raw_sse: bool,
    max_backoff: Duration,
}

/// Wire shape of `POST /v1/messages`.
#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "<[SystemBlock]>::is_empty")]
    system: &'a [SystemBlock],
    messages: &'a [Message],
    #[serde(skip_serializing_if = "<[ToolDef]>::is_empty")]
    tools: &'a [ToolDef],
    thinking: &'a Thinking,
    output_config: OutputConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<&'a Metadata>,
}

#[derive(Serialize)]
struct OutputConfig {
    effort: Effort,
}

/// Wire shape of `POST /v1/messages/count_tokens`.
#[derive(Serialize)]
struct WireCountRequest<'a> {
    model: &'a str,
    #[serde(skip_serializing_if = "<[SystemBlock]>::is_empty")]
    system: &'a [SystemBlock],
    messages: &'a [Message],
    #[serde(skip_serializing_if = "<[ToolDef]>::is_empty")]
    tools: &'a [ToolDef],
    thinking: &'a Thinking,
}

impl AnthropicMentor {
    /// Builds an adapter from config and the API key. `http` should come
    /// from [`Self::http_client`] (or a test client).
    pub fn new(config: &MentorConfig, key: Secret, http: reqwest::Client) -> Self {
        Self {
            model: config.model.clone(),
            base_url: config.base_url.trim_end_matches('/').to_owned(),
            max_retries: config.max_retries,
            key,
            http,
            capture_raw_sse: false,
            max_backoff: MAX_BACKOFF,
        }
    }

    /// Caps every retry delay (including `retry-after`); tests use this.
    #[must_use]
    pub fn with_max_backoff(mut self, cap: Duration) -> Self {
        self.max_backoff = cap;
        self
    }

    /// Keep the raw SSE bytes in every response (`trace.capture_raw_sse`).
    #[must_use]
    pub fn with_raw_sse(mut self, on: bool) -> Self {
        self.capture_raw_sse = on;
        self
    }

    /// A client with the timeouts the adapter expects.
    ///
    /// # Errors
    /// TLS backend initialisation failed.
    pub fn http_client(config: &MentorConfig) -> Result<reqwest::Client, reqwest::Error> {
        reqwest::Client::builder()
            .user_agent(format!("apprentice-harness/{}", crate::VERSION))
            .timeout(Duration::from_secs(config.timeout_s))
            // The API pings every few seconds while thinking; a quiet
            // stream this long is dead.
            .read_timeout(Duration::from_secs(90))
            .connect_timeout(Duration::from_secs(10))
            .build()
    }

    /// The exact JSON body sent for `req` (for traces and snapshots).
    ///
    /// # Errors
    /// Serialisation failure (should not happen for well-formed types).
    pub fn request_body(&self, req: &MentorRequest) -> Result<Vec<u8>, MentorError> {
        serde_json::to_vec(&WireRequest {
            model: &req.model,
            max_tokens: req.max_tokens,
            stream: true,
            system: &req.system,
            messages: &req.messages,
            tools: &req.tools,
            thinking: &req.thinking,
            output_config: OutputConfig { effort: req.effort },
            metadata: req.metadata.as_ref(),
        })
        .map_err(|e| MentorError::Protocol(format!("cannot serialise request: {e}")))
    }

    fn post(&self, path: &str, body: Vec<u8>) -> reqwest::RequestBuilder {
        self.http
            .post(format!("{}{path}", self.base_url))
            .header("content-type", "application/json")
            .header("x-api-key", self.key.expose())
            .header("anthropic-version", ANTHROPIC_VERSION)
            .body(body)
    }

    /// One streaming attempt. `Err` carries whether content had started
    /// (in the `StreamInterrupted` variant) so the caller can decide on a
    /// retry.
    async fn attempt(
        &self,
        body: Vec<u8>,
        on_event: &mut EventSink<'_>,
        cancel: &CancellationToken,
        started: Instant,
    ) -> Result<(Assembler, u64, Option<Vec<u8>>), MentorError> {
        let send = self.post("/v1/messages", body).send();
        let response = tokio::select! {
            () = cancel.cancelled() => return Err(MentorError::Cancelled),
            r = send => r.map_err(map_reqwest)?,
        };
        let first_byte_ms = elapsed_ms(started);
        let status = response.status();
        if status != StatusCode::OK {
            let retry_after = parse_retry_after(response.headers());
            let text = response.text().await.unwrap_or_default();
            return Err(status_error(status, retry_after, &text));
        }

        let mut stream = response.bytes_stream();
        let mut parser = SseParser::new();
        let mut asm = Assembler::new();
        let mut raw = self.capture_raw_sse.then(Vec::new);
        loop {
            let chunk = tokio::select! {
                () = cancel.cancelled() => return Err(MentorError::Cancelled),
                c = stream.next() => c,
            };
            match chunk {
                Some(Ok(bytes)) => {
                    if let Some(raw) = raw.as_mut() {
                        raw.extend_from_slice(&bytes);
                    }
                    for ev in parser.push(&bytes) {
                        for out in feed(&mut asm, &ev.data)? {
                            on_event(out);
                        }
                    }
                    if asm.finished {
                        return Ok((asm, first_byte_ms, raw));
                    }
                }
                Some(Err(e)) => {
                    return Err(if asm.content_started() {
                        MentorError::StreamInterrupted {
                            partial: asm.content(),
                        }
                    } else if e.is_timeout() {
                        MentorError::Timeout
                    } else {
                        // Includes body decode errors from an abrupt close
                        // (hyper reports those as decode, not I/O).
                        tracing::debug!(error = %e, "stream failed before content");
                        MentorError::Disconnected
                    });
                }
                None => {
                    if let Some(ev) = parser.finish() {
                        for out in feed(&mut asm, &ev.data)? {
                            on_event(out);
                        }
                    }
                    if asm.finished {
                        return Ok((asm, first_byte_ms, raw));
                    }
                    return Err(if asm.content_started() {
                        MentorError::StreamInterrupted {
                            partial: asm.content(),
                        }
                    } else {
                        MentorError::Disconnected
                    });
                }
            }
        }
    }

    async fn backoff(
        &self,
        attempt: u32,
        err: &MentorError,
        cancel: &CancellationToken,
    ) -> Result<(), MentorError> {
        let delay = if let MentorError::RateLimited {
            retry_after: Some(d),
        } = err
        {
            (*d).min(self.max_backoff)
        } else {
            let base = Duration::from_secs(1u64 << attempt.min(6));
            let jitter = jitter_fraction();
            base.mul_f64(0.75 + jitter * 0.5).min(self.max_backoff)
        };
        tracing::warn!(
            attempt,
            delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            error = %err,
            "mentor call failed; retrying"
        );
        tokio::select! {
            () = cancel.cancelled() => Err(MentorError::Cancelled),
            () = tokio::time::sleep(delay) => Ok(()),
        }
    }
}

#[async_trait]
impl Mentor for AnthropicMentor {
    async fn complete(
        &self,
        req: &MentorRequest,
        on_event: &mut EventSink<'_>,
        cancel: CancellationToken,
    ) -> Result<MentorResponse, MentorError> {
        let body = self.request_body(req)?;
        let request_bytes = body.len();
        let started = Instant::now();
        let span = tracing::info_span!("mentor", model = %req.model, request_bytes);
        let _enter = span.enter();

        let mut attempt = 0u32;
        loop {
            let result = self.attempt(body.clone(), on_event, &cancel, started).await;
            match result {
                Ok((asm, first_byte_ms, raw_sse)) => {
                    let total_ms = elapsed_ms(started);
                    tracing::info!(
                        first_byte_ms,
                        total_ms,
                        attempts = attempt + 1,
                        input = asm.usage.input_tokens,
                        output = asm.usage.output_tokens,
                        cache_read = asm.usage.cache_read_input_tokens,
                        cache_creation = asm.usage.cache_creation_input_tokens,
                        stop_reason = ?asm.stop_reason(),
                        "mentor call complete"
                    );
                    return Ok(MentorResponse {
                        id: asm.id.clone(),
                        model: asm.model.clone(),
                        content: asm.content(),
                        stop_reason: asm.stop_reason(),
                        stop_details: asm.stop_details.clone(),
                        usage: asm.usage,
                        timing: Timing {
                            first_byte_ms,
                            total_ms,
                            attempts: attempt + 1,
                        },
                        request_bytes,
                        raw_sse,
                    });
                }
                Err(e) if e.is_retryable() && attempt < self.max_retries => {
                    self.backoff(attempt, &e, &cancel).await?;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn count_tokens(&self, req: &MentorRequest) -> Result<u64, MentorError> {
        let body = serde_json::to_vec(&WireCountRequest {
            model: &req.model,
            system: &req.system,
            messages: &req.messages,
            tools: &req.tools,
            thinking: &req.thinking,
        })
        .map_err(|e| MentorError::Protocol(format!("cannot serialise request: {e}")))?;
        let cancel = CancellationToken::new();
        let mut attempt = 0u32;
        loop {
            let result: Result<u64, MentorError> = async {
                let response = self
                    .post("/v1/messages/count_tokens", body.clone())
                    .send()
                    .await
                    .map_err(map_reqwest)?;
                let status = response.status();
                if status != StatusCode::OK {
                    let retry_after = parse_retry_after(response.headers());
                    let text = response.text().await.unwrap_or_default();
                    return Err(status_error(status, retry_after, &text));
                }
                let v: serde_json::Value = response.json().await.map_err(map_reqwest)?;
                v.get("input_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| MentorError::Protocol("count_tokens: no input_tokens".into()))
            }
            .await;
            match result {
                Ok(n) => return Ok(n),
                Err(e) if e.is_retryable() && attempt < self.max_retries => {
                    self.backoff(attempt, &e, &cancel).await?;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn model_id(&self) -> &str {
        &self.model
    }
}

/// Feeds one event; an API `error` event after content has started is an
/// interruption (the partial output must not be silently retried).
fn feed(asm: &mut Assembler, data: &str) -> Result<Vec<super::types::StreamEvent>, MentorError> {
    match asm.feed(data) {
        Err(MentorError::Stream { kind, message }) if asm.content_started() => {
            tracing::warn!(kind, message, "stream error after content started");
            Err(MentorError::StreamInterrupted {
                partial: asm.content(),
            })
        }
        other => other,
    }
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Pseudo-random in `[0, 1)` from the clock; enough to spread retries.
fn jitter_fraction() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    f64::from(nanos % 1000) / 1000.0
}

fn map_reqwest(e: reqwest::Error) -> MentorError {
    if e.is_timeout() {
        MentorError::Timeout
    } else {
        MentorError::Network(e)
    }
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

fn status_error(status: StatusCode, retry_after: Option<Duration>, body: &str) -> MentorError {
    let parsed: Option<ApiErrorBody> = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| serde_json::from_value(v.get("error")?.clone()).ok());
    let (kind, message) = parsed.map_or_else(
        || ("unknown".to_owned(), body.chars().take(500).collect()),
        |e| (e.kind, e.message),
    );
    match status.as_u16() {
        401 | 403 if kind != "billing_error" => MentorError::Auth,
        400 => MentorError::InvalidRequest { message },
        413 => MentorError::RequestTooLarge,
        429 => MentorError::RateLimited { retry_after },
        529 => MentorError::Overloaded,
        s => MentorError::Api {
            status: s,
            kind,
            message,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping() {
        let body = r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#;
        assert!(matches!(
            status_error(StatusCode::TOO_MANY_REQUESTS, Some(Duration::from_secs(2)), body),
            MentorError::RateLimited { retry_after: Some(d) } if d.as_secs() == 2
        ));
        assert!(matches!(
            status_error(StatusCode::UNAUTHORIZED, None, "{}"),
            MentorError::Auth
        ));
        assert!(matches!(
            status_error(StatusCode::FORBIDDEN, None, r#"{"error":{"type":"billing_error","message":"pay"}}"#),
            MentorError::Api { status: 403, kind, .. } if kind == "billing_error"
        ));
        assert!(matches!(
            status_error(StatusCode::from_u16(529).unwrap(), None, "overloaded"),
            MentorError::Overloaded
        ));
        assert!(matches!(
            status_error(StatusCode::BAD_REQUEST, None, r#"{"error":{"type":"invalid_request_error","message":"bad"}}"#),
            MentorError::InvalidRequest { message } if message == "bad"
        ));
    }

    #[test]
    fn retry_after_header() {
        let mut h = reqwest::header::HeaderMap::new();
        assert_eq!(parse_retry_after(&h), None);
        h.insert("retry-after", "7".parse().unwrap());
        assert_eq!(parse_retry_after(&h), Some(Duration::from_secs(7)));
        h.insert(
            "retry-after",
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(parse_retry_after(&h), None);
    }
}
