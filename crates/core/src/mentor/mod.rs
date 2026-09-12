//! The mentor: a remote foundation model behind the Messages API.
//!
//! [`Mentor`] is the trait the runtime calls; [`AnthropicMentor`] is the
//! only implementation for now. The adapter is pure: it neither writes
//! traces nor executes tools — the runtime records around it.

mod anthropic;
mod assemble;
mod error;
mod sse;
pub mod types;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

pub use anthropic::{ANTHROPIC_VERSION, AnthropicMentor};
pub use error::MentorError;
pub use sse::{SseEvent, SseParser};
pub use types::{
    CacheFlag, ContentBlock, Effort, MentorRequest, MentorResponse, Message, Metadata, Role,
    StopDetails, StopReason, StreamEvent, SystemBlock, Thinking, ThinkingDisplay, Timing, ToolDef,
    ToolResultContent, Usage,
};

/// Receives streaming events; must be cheap (forward to a channel).
pub type EventSink<'a> = dyn FnMut(StreamEvent) + Send + 'a;

/// A remote model.
#[async_trait]
pub trait Mentor: Send + Sync {
    /// Streams one completion, calling `on_event` for every delta, and
    /// returns the assembled response.
    ///
    /// # Errors
    /// See [`MentorError`]; `Cancelled` when `cancel` fires.
    async fn complete(
        &self,
        req: &MentorRequest,
        on_event: &mut EventSink<'_>,
        cancel: CancellationToken,
    ) -> Result<MentorResponse, MentorError>;

    /// Input token count for `req` as the API would bill it.
    ///
    /// # Errors
    /// See [`MentorError`].
    async fn count_tokens(&self, req: &MentorRequest) -> Result<u64, MentorError>;

    /// Configured model id.
    fn model_id(&self) -> &str;
}
