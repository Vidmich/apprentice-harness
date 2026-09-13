//! Prompts over RPC: the table of pending `permission.request`s and the
//! [`Prompter`] that emits them on an agent's event stream and waits
//! for `permission.respond`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apprentice_api::events::Event;
use apprentice_api::jsonrpc::RpcError;
use apprentice_api::types::{PermissionAnswer, RuleSpec};
use async_trait::async_trait;
use tokio::sync::{broadcast, oneshot};
use tracing::debug;

use super::{Ask, Prompter, Reply, SessionRules};
use crate::trace::SessionId;

/// Where an agent's live events go. `emit` says whether anyone is
/// listening: with nobody, a prompt is headless.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: Event) -> bool;
}

impl EventSink for broadcast::Sender<Event> {
    fn emit(&self, event: Event) -> bool {
        self.send(event).is_ok()
    }
}

struct Pending {
    agent_id: String,
    tx: oneshot::Sender<(PermissionAnswer, Option<RuleSpec>)>,
}

/// One per process: the prompts waiting for an answer, and the
/// remembered answers of every session.
#[derive(Default)]
pub struct PermissionBroker {
    pending: Mutex<HashMap<String, Pending>>,
    sessions: Mutex<HashMap<SessionId, Arc<SessionRules>>>,
}

impl std::fmt::Debug for PermissionBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionBroker")
            .field("pending", &self.pending().len())
            .finish_non_exhaustive()
    }
}

impl PermissionBroker {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Pending>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The `allow_session` rules of `session` (created on first use).
    pub fn session_rules(&self, session: &SessionId) -> Arc<SessionRules> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(session.clone())
            .or_default()
            .clone()
    }

    /// Forgets the remembered answers of `session`.
    pub fn forget_session(&self, session: &SessionId) {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session);
    }

    /// `(request_id, agent_id)` of every prompt waiting for an answer.
    pub fn pending(&self) -> Vec<(String, String)> {
        self.lock()
            .iter()
            .map(|(id, p)| (id.clone(), p.agent_id.clone()))
            .collect()
    }

    fn register(
        &self,
        request_id: &str,
        agent_id: &str,
    ) -> oneshot::Receiver<(PermissionAnswer, Option<RuleSpec>)> {
        let (tx, rx) = oneshot::channel();
        self.lock().insert(
            request_id.to_owned(),
            Pending {
                agent_id: agent_id.to_owned(),
                tx,
            },
        );
        rx
    }

    fn forget(&self, request_id: &str) {
        self.lock().remove(request_id);
    }

    /// `permission.respond`: the first answer settles the request.
    ///
    /// # Errors
    /// `not_found` when no such prompt is waiting (answered already,
    /// timed out, or never asked).
    pub fn respond(
        &self,
        request_id: &str,
        answer: PermissionAnswer,
        rule: Option<RuleSpec>,
    ) -> Result<(), RpcError> {
        let pending = self.lock().remove(request_id).ok_or_else(|| {
            RpcError::not_found(format!(
                "no permission request {request_id} is waiting for an answer"
            ))
        })?;
        debug!(request = request_id, ?answer, "permission answered");
        // A receiver gone means the agent stopped waiting (cancelled);
        // the answer is simply late.
        let _ = pending.tx.send((answer, rule));
        Ok(())
    }
}

/// Emits `permission.request` on `sink` and waits on the broker.
pub struct AgentPrompter {
    broker: Arc<PermissionBroker>,
    sink: Arc<dyn EventSink>,
}

impl std::fmt::Debug for AgentPrompter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentPrompter").finish_non_exhaustive()
    }
}

impl AgentPrompter {
    pub fn new(broker: Arc<PermissionBroker>, sink: Arc<dyn EventSink>) -> Self {
        Self { broker, sink }
    }
}

/// Drops the pending entry when the wait ends any way but an answer.
struct Forget<'a>(&'a PermissionBroker, &'a str);

impl Drop for Forget<'_> {
    fn drop(&mut self) {
        self.0.forget(self.1);
    }
}

#[async_trait]
impl Prompter for AgentPrompter {
    async fn ask(&self, ask: &Ask<'_>) -> Reply {
        let agent_id = ask.agent_id.to_string();
        let rx = self.broker.register(ask.request_id, &agent_id);
        let _forget = Forget(&self.broker, ask.request_id);
        let req = ask.request;
        let event = Event::PermissionRequest {
            request_id: ask.request_id.to_owned(),
            agent_id,
            tool: req.tool.clone(),
            input: req.input_view(),
            risk: req.risk,
            description: req.description.clone(),
            command: req.command.clone(),
            paths: req.shown_paths(),
            suggested_rules: ask.suggested.to_vec(),
            timeout_s: ask.timeout.as_secs(),
        };
        if !self.sink.emit(event) {
            return Reply::NoClient;
        }
        match tokio::time::timeout(ask.timeout.max(Duration::from_millis(1)), rx).await {
            Ok(Ok((answer, rule))) => Reply::Answered { answer, rule },
            // No answer in time, or the broker dropped the sender.
            Ok(Err(_)) | Err(_) => Reply::TimedOut,
        }
    }
}
