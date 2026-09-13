//! The `session.*` methods (task M01-10) over the store and the
//! runtime: create, list with activity and cost, read the stored
//! conversation, search it, archive, delete, rename, export. The
//! mutations refuse a session with an agent running on it and keep the
//! in-memory conversation in step with the store. [`title`] names
//! sessions.

pub mod title;

use std::sync::Arc;

use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::{
    Empty, SessionArchive, SessionArchiveParams, SessionCreate, SessionCreateParams, SessionDelete,
    SessionDeleteParams, SessionDeleteResult, SessionExportMethod, SessionGet, SessionGetParams,
    SessionGetResult, SessionIdParams, SessionList, SessionListParams, SessionListResult,
    SessionRename, SessionRenameParams, SessionSearch, SessionSearchParams, SessionSearchResult,
};
use apprentice_api::server::{Connection, Router};
use apprentice_api::types::SessionExport;
use serde_json::json;

use crate::app::AppState;
use crate::trace::{NewEvent, SessionId, SessionQuery, StoredMessage, TitleSource, kinds};

/// Messages `session.get` returns when `limit` is absent.
pub const DEFAULT_PAGE: u32 = 200;

/// Registers every `session.*` method.
pub fn register(state: &Arc<AppState>, router: &mut Router) {
    let s = Arc::clone(state);
    router.add::<SessionCreate, _, _>(move |_c: Arc<Connection>, p: SessionCreateParams| {
        let state = Arc::clone(&s);
        async move { state.session_create(&p).await }
    });
    let s = Arc::clone(state);
    router.add::<SessionList, _, _>(move |_c: Arc<Connection>, p: SessionListParams| {
        let state = Arc::clone(&s);
        async move { list(&state, &p).await }
    });
    let s = Arc::clone(state);
    router.add::<SessionGet, _, _>(move |_c: Arc<Connection>, p: SessionGetParams| {
        let state = Arc::clone(&s);
        async move { get(&state, &p).await }
    });
    let s = Arc::clone(state);
    router.add::<SessionSearch, _, _>(move |_c: Arc<Connection>, p: SessionSearchParams| {
        let state = Arc::clone(&s);
        async move { search(&state, &p).await }
    });
    let s = Arc::clone(state);
    router.add::<SessionArchive, _, _>(move |_c: Arc<Connection>, p: SessionArchiveParams| {
        let state = Arc::clone(&s);
        async move { archive(&state, &p).await }
    });
    let s = Arc::clone(state);
    router.add::<SessionDelete, _, _>(move |_c: Arc<Connection>, p: SessionDeleteParams| {
        let state = Arc::clone(&s);
        async move { delete(&state, &p).await }
    });
    let s = Arc::clone(state);
    router.add::<SessionRename, _, _>(move |_c: Arc<Connection>, p: SessionRenameParams| {
        let state = Arc::clone(&s);
        async move { rename(&state, &p).await }
    });
    let s = Arc::clone(state);
    router.add::<SessionExportMethod, _, _>(move |_c: Arc<Connection>, p: SessionIdParams| {
        let state = Arc::clone(&s);
        async move { export(&state, &p).await }
    });
}

/// `session.list`.
///
/// # Errors
/// Store failure.
pub async fn list(
    state: &Arc<AppState>,
    p: &SessionListParams,
) -> Result<SessionListResult, RpcError> {
    // The stored root is canonical; a caller's spelling may not be.
    let workspace = p.workspace.as_ref().map(|root| {
        crate::workspace::canonical_dir(std::path::Path::new(root))
            .map_or_else(|_| root.clone(), |c| c.to_string_lossy().into_owned())
    });
    let q = SessionQuery {
        query: p.query.clone(),
        workspace,
        workspace_id: p.workspace_id.as_deref().map(Into::into),
        include_archived: p.include_archived,
        limit: p.limit,
        offset: p.offset.unwrap_or(0),
    };
    let store = Arc::clone(state.store());
    let sessions = blocking(move || Ok(store.list_sessions(&q)?)).await?;
    Ok(SessionListResult { sessions })
}

/// `session.get`: the session and a page of its messages.
///
/// # Errors
/// Unknown session.
pub async fn get(
    state: &Arc<AppState>,
    p: &SessionGetParams,
) -> Result<SessionGetResult, RpcError> {
    let id = SessionId::from(p.id.as_str());
    let store = Arc::clone(state.store());
    let limit = p.limit.unwrap_or(DEFAULT_PAGE).max(1);
    let after = p.after_seq.unwrap_or(0);
    blocking(move || {
        let session = store.session_info(&id)?;
        // One past the page tells whether more follow.
        let mut rows = store.session_messages(&id, after, Some(limit + 1))?;
        let has_more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let messages = rows
            .into_iter()
            .map(StoredMessage::into_wire)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SessionGetResult {
            session,
            messages,
            has_more,
        })
    })
    .await
}

/// `session.search`.
///
/// # Errors
/// Store failure (a query the index rejects is an empty result, not
/// an error).
pub async fn search(
    state: &Arc<AppState>,
    p: &SessionSearchParams,
) -> Result<SessionSearchResult, RpcError> {
    let store = Arc::clone(state.store());
    let (query, archived, limit) = (p.query.clone(), p.include_archived, p.limit);
    let hits = blocking(move || Ok(store.search_sessions(&query, archived, limit)?)).await?;
    Ok(SessionSearchResult { hits })
}

/// `session.archive`. An archived session's conversation leaves
/// memory; a run on it later loads it back.
///
/// # Errors
/// Unknown session; `conflict` while an agent runs on it.
pub async fn archive(state: &Arc<AppState>, p: &SessionArchiveParams) -> Result<Empty, RpcError> {
    let id = SessionId::from(p.id.as_str());
    ensure_idle(state, &id)?;
    let archived = p.archived;
    let sid = id.clone();
    state
        .writer()
        .run(move |store| store.archive_session(&sid, archived))
        .await?;
    if archived {
        state.agents().forget_conversation(&id);
    }
    Ok(Empty {})
}

/// `session.delete`.
///
/// # Errors
/// Unknown session; `conflict` while an agent runs on it.
pub async fn delete(
    state: &Arc<AppState>,
    p: &SessionDeleteParams,
) -> Result<SessionDeleteResult, RpcError> {
    let id = SessionId::from(p.id.as_str());
    ensure_idle(state, &id)?;
    state.agents().forget_conversation(&id);
    let purge = p.purge_traces;
    let sid = id.clone();
    let report = state
        .writer()
        .run(move |store| store.delete_session(&sid, purge))
        .await?;
    Ok(SessionDeleteResult {
        messages_deleted: report.messages,
        events_deleted: report.events,
    })
}

/// `session.rename`: the user's title, recorded as `session.title`.
///
/// # Errors
/// Unknown session; an empty title (`invalid_params`).
pub async fn rename(state: &Arc<AppState>, p: &SessionRenameParams) -> Result<Empty, RpcError> {
    let title = p.title.trim().to_owned();
    if title.is_empty() {
        return Err(RpcError::invalid_params("the title is empty"));
    }
    let id = SessionId::from(p.id.as_str());
    state
        .writer()
        .run(move |store| {
            store.set_session_title(&id, Some(&title), TitleSource::User)?;
            store.append(NewEvent::new(id, kinds::SESSION_TITLE).payload(json!({
                "title": title,
                "source": TitleSource::User,
            })))
        })
        .await?;
    Ok(Empty {})
}

/// `session.export`.
///
/// # Errors
/// Unknown session.
pub async fn export(state: &Arc<AppState>, p: &SessionIdParams) -> Result<SessionExport, RpcError> {
    let id = SessionId::from(p.id.as_str());
    let store = Arc::clone(state.store());
    blocking(move || Ok(store.export_session(&id)?)).await
}

/// `conflict` while an agent runs on `session`.
fn ensure_idle(state: &AppState, session: &SessionId) -> Result<(), RpcError> {
    match state.agents().running_on(session) {
        Some(running) => Err(RpcError::conflict(format!(
            "agent {} is still running on session {session}",
            running.agent_id
        ))
        .with_details(json!({ "agent_id": running.agent_id.to_string() }))),
        None => Ok(()),
    }
}

async fn blocking<T, F>(f: F) -> Result<T, RpcError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, RpcError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| RpcError::internal(format!("session task failed: {e}")))?
}
