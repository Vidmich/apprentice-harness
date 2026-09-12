//! `session.list`, `trace.list` and `trace.get` over the store. The daemon
//! registers them on its router (task M00-08); `session.create` belongs to
//! the runtime, which snapshots the resolved config.

use std::sync::Arc;

use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::{
    SessionList, SessionListParams, SessionListResult, TraceGet, TraceGetParams, TraceGetResult,
    TraceList, TraceListParams, TraceListResult,
};
use apprentice_api::server::{Connection, Router};

use super::store::{EventQuery, TraceStore};
use super::{BlobId, EventId};

/// Query-side RPC handlers.
#[derive(Debug, Clone)]
pub struct TraceService {
    store: Arc<TraceStore>,
}

impl TraceService {
    pub fn new(store: Arc<TraceStore>) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &Arc<TraceStore> {
        &self.store
    }

    pub fn session_list(&self, p: &SessionListParams) -> Result<SessionListResult, RpcError> {
        let sessions = self.store.list_sessions(p.limit, p.offset.unwrap_or(0))?;
        Ok(SessionListResult { sessions })
    }

    pub fn trace_list(&self, p: &TraceListParams) -> Result<TraceListResult, RpcError> {
        let q = EventQuery {
            session_id: p.session_id.as_deref().map(Into::into),
            agent_id: p.agent_id.as_deref().map(Into::into),
            step_id: None,
            kinds: p.kinds.clone(),
            before_seq: p.before_seq,
            after_seq: None,
            limit: p.limit,
        };
        let events = self.store.list_events(&q)?;
        Ok(TraceListResult { events })
    }

    /// # Errors
    /// `not_found` for an unknown event (or a blob whose file was pruned).
    pub fn trace_get(&self, p: &TraceGetParams) -> Result<TraceGetResult, RpcError> {
        let event = self.store.get_event(&EventId::from(p.event_id.as_str()))?;
        let blob = match (&event.blob_id, p.include_blob) {
            (Some(id), true) => {
                let bytes = self.store.read_blob(&BlobId::from(id.as_str()))?;
                Some(String::from_utf8_lossy(&bytes).into_owned())
            }
            _ => None,
        };
        Ok(TraceGetResult { event, blob })
    }

    /// Registers the three query methods; each runs on the blocking pool.
    pub fn register(self: Arc<Self>, router: &mut Router) {
        let svc = Arc::clone(&self);
        router.add::<SessionList, _, _>(move |_c: Arc<Connection>, p: SessionListParams| {
            let svc = Arc::clone(&svc);
            blocking(move || svc.session_list(&p))
        });
        let svc = Arc::clone(&self);
        router.add::<TraceList, _, _>(move |_c: Arc<Connection>, p: TraceListParams| {
            let svc = Arc::clone(&svc);
            blocking(move || svc.trace_list(&p))
        });
        let svc = Arc::clone(&self);
        router.add::<TraceGet, _, _>(move |_c: Arc<Connection>, p: TraceGetParams| {
            let svc = Arc::clone(&svc);
            blocking(move || svc.trace_get(&p))
        });
    }
}

async fn blocking<T, F>(f: F) -> Result<T, RpcError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, RpcError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| RpcError::internal(format!("trace task failed: {e}")))?
}
