//! The hook points of the loop (SPEC §4): where the apprentice's roles
//! plug in once M03 implements them. The runtime calls them at every
//! step; [`NoopHooks`] does nothing, which is the remote-only baseline
//! the apprentice is measured against.

use async_trait::async_trait;

use super::conversation::Conversation;
use crate::tools::{Executed, ToolCall};

/// What a hook sees before the mentor is called. The compactor and the
/// context selector edit the conversation here; the gate may decide
/// that no call is needed at all (later).
#[derive(Debug)]
pub struct CallContext<'a> {
    pub conversation: &'a mut Conversation,
    /// The step about to start, from 1.
    pub step: u64,
}

/// What a hook sees before the tools of a step run. The executor role
/// takes calls it can do locally out of the list (later).
#[derive(Debug)]
pub struct ToolExecContext<'a> {
    pub calls: &'a mut Vec<ToolCall>,
    pub step: u64,
}

/// What a hook sees once the tools of a step ran and before their
/// results go back to the mentor. The output compressor rewrites the
/// blocks here.
#[derive(Debug)]
pub struct ToolResultContext<'a> {
    pub results: &'a mut Vec<Executed>,
    pub step: u64,
}

/// The three hook points. Every method has a no-op default.
#[async_trait]
pub trait StepHooks: Send + Sync {
    async fn before_call(&self, _ctx: &mut CallContext<'_>) {}

    async fn before_tool_exec(&self, _ctx: &mut ToolExecContext<'_>) {}

    async fn on_tool_result(&self, _ctx: &mut ToolResultContext<'_>) {}
}

/// The baseline: nothing between the mentor and its tools.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopHooks;

impl StepHooks for NoopHooks {}
