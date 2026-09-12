//! Server-side request router used by the daemon.
//!
//! The router owns no I/O policy beyond framing: it serves one connection at
//! a time per [`Router::serve`] call over any `AsyncRead`/`AsyncWrite` pair,
//! which keeps it testable with in-memory duplex streams.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinSet;
use tracing::{debug, warn};

use crate::API_VERSION;
use crate::codec::{Frame, read_frame, write_message};
use crate::events::{EVENT_METHOD, Event, EventNotification};
use crate::jsonrpc::{Id, Message, Notification, Request, Response, RpcError};
use crate::methods::{DaemonHello, HelloParams, HelloResult, Method};

/// Static facts the router reports in the handshake.
#[derive(Debug, Clone)]
pub struct RouterConfig {
    pub daemon_version: String,
    pub pid: u32,
    /// When `Some`, `daemon.hello` must carry exactly this token.
    pub token: Option<String>,
}

type HandlerFuture = Pin<Box<dyn Future<Output = Result<Value, RpcError>> + Send>>;
type Handler = Arc<dyn Fn(Arc<Connection>, Value) -> HandlerFuture + Send + Sync>;

/// Maps method names to handlers and runs connections.
pub struct Router {
    config: RouterConfig,
    handlers: HashMap<&'static str, Handler>,
    next_conn_id: AtomicU64,
}

impl std::fmt::Debug for Router {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Router")
            .field("methods", &self.handlers.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// Errors from serving a connection.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("connection I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Per-connection state handed to handlers.
#[derive(Debug)]
pub struct Connection {
    pub id: u64,
    authenticated: AtomicBool,
    client: Mutex<Option<String>>,
    outgoing: mpsc::Sender<Message>,
    seqs: Mutex<HashMap<String, u64>>,
}

/// The connection's outgoing queue is gone (peer disconnected).
#[derive(Debug, thiserror::Error)]
#[error("connection {0} closed")]
pub struct ConnectionClosed(pub u64);

impl Connection {
    pub fn is_authenticated(&self) -> bool {
        self.authenticated.load(Ordering::Acquire)
    }

    /// Client name from the handshake, if any.
    pub async fn client(&self) -> Option<String> {
        self.client.lock().await.clone()
    }

    /// Sends an event on a subscription, waiting for queue space.
    ///
    /// # Errors
    /// Returns [`ConnectionClosed`] when the peer is gone.
    pub async fn notify(&self, subscription: &str, event: Event) -> Result<(), ConnectionClosed> {
        let params = self.event_params(subscription, event).await;
        let msg = Message::notification(EVENT_METHOD, Some(serde_json::to_value(params).unwrap()));
        self.outgoing
            .send(msg)
            .await
            .map_err(|_| ConnectionClosed(self.id))
    }

    async fn event_params(&self, subscription: &str, event: Event) -> EventNotification {
        let mut seqs = self.seqs.lock().await;
        let seq = seqs.entry(subscription.to_owned()).or_insert(0);
        *seq += 1;
        EventNotification {
            subscription: subscription.to_owned(),
            seq: *seq,
            event,
        }
    }

    async fn respond(&self, response: Response) {
        if self
            .outgoing
            .send(Message::Response(response))
            .await
            .is_err()
        {
            debug!(conn = self.id, "dropping response: connection closed");
        }
    }
}

impl Router {
    pub fn new(config: RouterConfig) -> Self {
        Self {
            config,
            handlers: HashMap::new(),
            next_conn_id: AtomicU64::new(1),
        }
    }

    /// Registers a typed handler for `M`. Registering the same method twice
    /// replaces the earlier handler.
    pub fn add<M, F, Fut>(&mut self, handler: F) -> &mut Self
    where
        M: Method,
        F: Fn(Arc<Connection>, M::Params) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<M::Result, RpcError>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let erased: Handler = Arc::new(move |conn: Arc<Connection>, params: Value| {
            let handler = Arc::clone(&handler);
            Box::pin(async move {
                let params: M::Params = serde_json::from_value(params)
                    .map_err(|e| RpcError::invalid_params(format!("{}: {e}", M::NAME)))?;
                let result = handler(conn, params).await?;
                serde_json::to_value(result).map_err(|e| RpcError::internal(e.to_string()))
            })
        });
        self.handlers.insert(M::NAME, erased);
        self
    }

    pub fn has(&self, method: &str) -> bool {
        self.handlers.contains_key(method)
    }

    /// Registered method names (sorted), for parity checks.
    pub fn methods(&self) -> Vec<&'static str> {
        let mut v: Vec<_> = self.handlers.keys().copied().collect();
        v.sort_unstable();
        v
    }

    /// Serves one connection until the peer closes it or I/O fails.
    ///
    /// # Errors
    /// Returns [`ServeError::Io`] on transport failure.
    pub async fn serve<R, W>(self: Arc<Self>, reader: R, writer: W) -> Result<(), ServeError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (tx, mut rx) = mpsc::channel::<Message>(4096);
        let conn = Arc::new(Connection {
            id: self.next_conn_id.fetch_add(1, Ordering::Relaxed),
            authenticated: AtomicBool::new(false),
            client: Mutex::new(None),
            outgoing: tx,
            seqs: Mutex::new(HashMap::new()),
        });
        debug!(conn = conn.id, "connection opened");

        let mut writer = writer;
        let writer_task = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Err(e) = write_message(&mut writer, &msg).await {
                    debug!(error = %e, "writer stopped");
                    break;
                }
            }
        });

        let mut reader = BufReader::new(reader);
        let mut tasks: JoinSet<()> = JoinSet::new();
        let result = loop {
            tokio::select! {
                frame = read_frame(&mut reader) => match frame {
                    Ok(Frame::Line(line)) => self.handle_line(&conn, &line, &mut tasks).await,
                    Ok(Frame::TooLong { bytes_discarded }) => {
                        conn.respond(Response::failure(
                            None,
                            RpcError::invalid_request(format!(
                                "line of {bytes_discarded} bytes exceeds the {} byte limit",
                                crate::codec::MAX_LINE_BYTES
                            )),
                        ))
                        .await;
                    }
                    Ok(Frame::Eof) => break Ok(()),
                    Err(e) => break Err(ServeError::Io(e)),
                },
                Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
            }
        };

        tasks.abort_all();
        drop(conn);
        let _ = writer_task.await;
        debug!("connection closed");
        result
    }

    async fn handle_line(
        self: &Arc<Self>,
        conn: &Arc<Connection>,
        line: &[u8],
        tasks: &mut JoinSet<()>,
    ) {
        if line.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        if line.trim_ascii_start().first() == Some(&b'[') {
            conn.respond(Response::failure(
                None,
                RpcError::invalid_request("batch requests are not supported"),
            ))
            .await;
            return;
        }
        let message: Message = match serde_json::from_slice(line) {
            Ok(m) => m,
            Err(e) => {
                conn.respond(Response::failure(
                    None,
                    RpcError::parse_error(e.to_string()),
                ))
                .await;
                return;
            }
        };
        match message {
            Message::Request(req) => self.dispatch(conn, req, tasks).await,
            Message::Notification(Notification { method, .. }) => {
                debug!(conn = conn.id, method, "ignoring client notification");
            }
            Message::Response(_) => {
                debug!(conn = conn.id, "ignoring response sent by client");
            }
        }
    }

    async fn dispatch(
        self: &Arc<Self>,
        conn: &Arc<Connection>,
        req: Request,
        tasks: &mut JoinSet<()>,
    ) {
        let Request {
            id, method, params, ..
        } = req;
        let params = params.unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        let params = if params.is_null() {
            Value::Object(serde_json::Map::new())
        } else {
            params
        };

        if method == DaemonHello::NAME {
            let response = match self.hello(conn, params).await {
                Ok(v) => Response::success(id, v),
                Err(e) => Response::failure(Some(id), e),
            };
            conn.respond(response).await;
            return;
        }
        if !conn.is_authenticated() {
            conn.respond(Response::failure(
                Some(id),
                RpcError::unauthorized("send daemon.hello before any other method"),
            ))
            .await;
            return;
        }
        let Some(handler) = self.handlers.get(method.as_str()).cloned() else {
            conn.respond(Response::failure(
                Some(id),
                RpcError::method_not_found(&method),
            ))
            .await;
            return;
        };

        let conn = Arc::clone(conn);
        tasks.spawn(async move {
            let inner = tokio::spawn(handler(Arc::clone(&conn), params));
            let outcome = match inner.await {
                Ok(r) => r,
                Err(e) if e.is_panic() => {
                    warn!(conn = conn.id, method, "handler panicked");
                    Err(RpcError::internal(format!("handler for {method} panicked")))
                }
                Err(_) => Err(RpcError::cancelled()),
            };
            let response = match outcome {
                Ok(v) => Response::success(id, v),
                Err(e) => Response::failure(Some(id), e),
            };
            conn.respond(response).await;
        });
    }

    async fn hello(&self, conn: &Arc<Connection>, params: Value) -> Result<Value, RpcError> {
        let p: HelloParams = serde_json::from_value(params)
            .map_err(|e| RpcError::invalid_params(format!("daemon.hello: {e}")))?;
        if p.api_version != API_VERSION {
            return Err(RpcError::incompatible_api(p.api_version, API_VERSION));
        }
        if let Some(expected) = &self.config.token
            && p.token.as_deref() != Some(expected.as_str())
        {
            return Err(RpcError::unauthorized("invalid daemon token"));
        }
        *conn.client.lock().await = Some(format!("{} {}", p.client, p.client_version));
        conn.authenticated.store(true, Ordering::Release);
        debug!(conn = conn.id, client = p.client, "handshake ok");
        let result = HelloResult {
            daemon_version: self.config.daemon_version.clone(),
            api_version: API_VERSION,
            pid: self.config.pid,
        };
        Ok(serde_json::to_value(result).unwrap())
    }
}

/// Convenience for handlers that want to answer with an `Id`-less error.
impl From<ConnectionClosed> for RpcError {
    fn from(e: ConnectionClosed) -> Self {
        RpcError::internal(e.to_string())
    }
}

/// Helper to build a `Response` for a request id in tests and handlers.
pub fn response_for(id: Id, result: Result<Value, RpcError>) -> Response {
    match result {
        Ok(v) => Response::success(id, v),
        Err(e) => Response::failure(Some(id), e),
    }
}
