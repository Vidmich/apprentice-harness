//! Client library used by the CLI and the GUI backend to talk to `harnessd`.
//!
//! [`DaemonClient`] speaks the `apprentice-api` protocol over any
//! `AsyncRead`/`AsyncWrite` pair: request/response with typed methods,
//! and per-subscription event streams. [`discovery`] reads and writes
//! `daemon.json`; [`DaemonClient::connect`] finds the daemon through it and
//! spawns `harnessd` when nothing answers.

pub mod connect;
pub mod discovery;

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};
use std::time::Duration;

use apprentice_api::API_VERSION;
use apprentice_api::codec::{Frame, read_frame, write_message};
use apprentice_api::events::{EVENT_METHOD, Event, EventNotification};
use apprentice_api::jsonrpc::{Id, Message, Response, RpcError};
use apprentice_api::methods::{DaemonHello, HasSubscription, HelloParams, HelloResult, Method};
use apprentice_api::transport::Endpoint;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{debug, warn};

pub use connect::{ConnectError, ConnectOptions, Connected, DAEMON_PATH_ENV};
pub use discovery::{DAEMON_INFO_FILE, DaemonInfo, DiscoveryError};

/// Crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Errors returned by [`DaemonClient`].
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The daemon answered with an error.
    #[error("{0}")]
    Rpc(#[from] RpcError),
    #[error("transport error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The connection is gone (daemon exited or closed the socket).
    #[error("connection to daemon closed")]
    Closed,
    #[error("request timed out after {0:?}")]
    Timeout(Duration),
}

/// Options for a client connection.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// Per-request timeout for `call`. `None` = wait forever.
    pub timeout: Option<Duration>,
    /// Capacity of each subscription's event channel.
    pub event_buffer: usize,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            timeout: Some(Duration::from_secs(60)),
            event_buffer: 1024,
        }
    }
}

/// Events for subscriptions nobody has subscribed to yet are held here so a
/// `call_streaming` that returns after the first events were emitted does not
/// lose them. Bounded across all subscriptions.
const PENDING_EVENT_CAP: usize = 1024;

struct Subscription {
    tx: mpsc::Sender<EventNotification>,
    dropped: u64,
}

struct Inner {
    outgoing: mpsc::Sender<Message>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>,
    subs: Mutex<HashMap<String, Subscription>>,
    unclaimed: Mutex<(usize, HashMap<String, VecDeque<EventNotification>>)>,
    next_id: AtomicU64,
    closed: AtomicBool,
    options: ClientOptions,
    /// The reader task, stopped when the last handle goes away.
    reader: std::sync::Mutex<Option<tokio::task::AbortHandle>>,
}

impl Drop for Inner {
    /// The last [`DaemonClient`] clone is gone: stop reading and let the
    /// writer finish, which closes the transport so the daemon sees EOF
    /// (and a `--stdio` daemon exits).
    fn drop(&mut self) {
        if let Some(reader) = self
            .reader
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            reader.abort();
        }
    }
}

/// A connection to the daemon. Cheap to clone; all clones share the
/// socket, which closes when the last one is dropped.
#[derive(Clone)]
pub struct DaemonClient {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for DaemonClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonClient")
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

impl DaemonClient {
    /// Wraps an already-open transport. No handshake is performed; call
    /// [`DaemonClient::hello`] next.
    pub fn from_streams<R, W>(reader: R, writer: W, options: ClientOptions) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (out_tx, mut out_rx) = mpsc::channel::<Message>(1024);
        let inner = Arc::new(Inner {
            outgoing: out_tx,
            pending: Mutex::new(HashMap::new()),
            subs: Mutex::new(HashMap::new()),
            unclaimed: Mutex::new((0, HashMap::new())),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            options,
            reader: std::sync::Mutex::new(None),
        });

        let mut writer = writer;
        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if let Err(e) = write_message(&mut writer, &msg).await {
                    debug!(error = %e, "client writer stopped");
                    break;
                }
            }
        });

        // The reader holds only a weak reference so that dropping every
        // client handle (not the daemon closing) also ends the connection.
        let weak: Weak<Inner> = Arc::downgrade(&inner);
        let reader_task = tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            loop {
                match read_frame(&mut reader).await {
                    Ok(Frame::Line(line)) => {
                        let Some(inner) = weak.upgrade() else { break };
                        inner.handle_line(&line).await;
                    }
                    Ok(Frame::TooLong { bytes_discarded }) => {
                        warn!(bytes_discarded, "daemon sent an oversized line; skipped");
                    }
                    Ok(Frame::Eof) => break,
                    Err(e) => {
                        debug!(error = %e, "client reader stopped");
                        break;
                    }
                }
            }
            if let Some(inner) = weak.upgrade() {
                inner.close().await;
            }
        });
        *inner
            .reader
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reader_task.abort_handle());

        Self { inner }
    }

    /// Connects to a daemon endpoint. No handshake is performed.
    ///
    /// # Errors
    /// Returns the OS error when nothing is listening.
    pub async fn connect_endpoint(
        endpoint: &Endpoint,
        options: ClientOptions,
    ) -> Result<Self, ClientError> {
        let (rx, tx) = endpoint.connect().await?;
        Ok(Self::from_streams(rx, tx, options))
    }

    /// Performs the `daemon.hello` handshake.
    ///
    /// # Errors
    /// `ClientError::Rpc` with kind `unauthorized` or `incompatible_api`, or a
    /// transport error.
    pub async fn hello(
        &self,
        client: &str,
        client_version: &str,
        token: Option<String>,
    ) -> Result<HelloResult, ClientError> {
        self.call::<DaemonHello>(HelloParams {
            client: client.to_owned(),
            client_version: client_version.to_owned(),
            api_version: API_VERSION,
            token,
        })
        .await
    }

    /// Sends a request and waits for its response.
    ///
    /// # Errors
    /// The daemon's error, a transport failure, or a timeout.
    pub async fn call<M: Method>(&self, params: M::Params) -> Result<M::Result, ClientError> {
        let value = self
            .call_raw(
                M::NAME,
                serde_json::to_value(params).map_err(|e| {
                    ClientError::Protocol(format!("cannot serialise params for {}: {e}", M::NAME))
                })?,
            )
            .await?;
        serde_json::from_value(value)
            .map_err(|e| ClientError::Protocol(format!("cannot parse result of {}: {e}", M::NAME)))
    }

    /// Untyped call, for generic bridges (the GUI's `rpc_call`).
    ///
    /// # Errors
    /// As [`DaemonClient::call`].
    pub async fn call_raw(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        if self.is_closed() {
            return Err(ClientError::Closed);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, tx);
        let msg = Message::request(Id::Number(id), method, Some(params));
        if self.inner.outgoing.send(msg).await.is_err() {
            self.inner.pending.lock().await.remove(&id);
            return Err(ClientError::Closed);
        }
        let outcome = match self.inner.options.timeout {
            Some(t) => {
                let Ok(r) = tokio::time::timeout(t, rx).await else {
                    self.inner.pending.lock().await.remove(&id);
                    return Err(ClientError::Timeout(t));
                };
                r
            }
            None => rx.await,
        };
        match outcome {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(ClientError::Rpc(e)),
            Err(_) => Err(ClientError::Closed),
        }
    }

    /// Calls a streaming method and returns its result together with the
    /// event stream of the subscription it names. Events emitted before this
    /// returns are not lost.
    ///
    /// # Errors
    /// As [`DaemonClient::call`].
    pub async fn call_streaming<M>(
        &self,
        params: M::Params,
    ) -> Result<(M::Result, EventStream), ClientError>
    where
        M: Method,
        M::Result: HasSubscription,
    {
        let result = self.call::<M>(params).await?;
        let stream = self.subscribe(result.subscription()).await;
        Ok((result, stream))
    }

    /// Subscribes to events for `subscription`. Buffered events emitted
    /// before the subscription are delivered first. The stream ends after the
    /// terminal event (`agent.finished`) or when the connection closes.
    pub async fn subscribe(&self, subscription: &str) -> EventStream {
        let (tx, rx) = mpsc::channel(self.inner.options.event_buffer.max(1));
        let backlog = {
            let mut unclaimed = self.inner.unclaimed.lock().await;
            let backlog = unclaimed.1.remove(subscription).unwrap_or_default();
            unclaimed.0 = unclaimed.0.saturating_sub(backlog.len());
            backlog
        };
        let mut dropped = 0u64;
        let mut terminal = None;
        for ev in backlog {
            if ev.event.is_terminal() {
                terminal = Some(ev);
                break;
            }
            if tx.try_send(ev).is_err() {
                dropped += 1;
            }
        }
        if let Some(ev) = terminal {
            deliver_terminal(tx, dropped, ev);
        } else if !self.is_closed() {
            self.inner
                .subs
                .lock()
                .await
                .insert(subscription.to_owned(), Subscription { tx, dropped });
        }
        EventStream { rx }
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }
}

impl Inner {
    async fn handle_line(&self, line: &[u8]) {
        let message: Message = match serde_json::from_slice(line) {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "unparseable line from daemon; skipped");
                return;
            }
        };
        match message {
            Message::Response(Response {
                id, result, error, ..
            }) => {
                let Some(Id::Number(id)) = id else {
                    debug!("response without a numeric id; ignored");
                    return;
                };
                let Some(tx) = self.pending.lock().await.remove(&id) else {
                    debug!(id, "response for unknown request; ignored");
                    return;
                };
                let outcome = match (result, error) {
                    (_, Some(e)) => Err(e),
                    (Some(v), None) => Ok(v),
                    (None, None) => Ok(Value::Null),
                };
                let _ = tx.send(outcome);
            }
            Message::Notification(n) if n.method == EVENT_METHOD => {
                let Some(params) = n.params else { return };
                match serde_json::from_value::<EventNotification>(params) {
                    Ok(ev) => self.route_event(ev).await,
                    Err(e) => warn!(error = %e, "malformed event; skipped"),
                }
            }
            Message::Notification(n) => debug!(method = n.method, "unknown notification; ignored"),
            Message::Request(r) => debug!(method = r.method, "daemon sent a request; ignored"),
        }
    }

    async fn route_event(&self, ev: EventNotification) {
        let key = ev.subscription.clone();
        let terminal = ev.event.is_terminal();
        let mut subs = self.subs.lock().await;
        if let Some(sub) = subs.get_mut(&key) {
            if terminal {
                let Subscription { tx, dropped } = subs.remove(&key).expect("present");
                deliver_terminal(tx, dropped, ev);
                return;
            }
            if sub.dropped > 0 && sub.tx.try_send(dropped_warning(&key, sub.dropped)).is_ok() {
                sub.dropped = 0;
            }
            if sub.tx.try_send(ev).is_err() {
                sub.dropped += 1;
            }
            return;
        }
        drop(subs);

        let mut unclaimed = self.unclaimed.lock().await;
        if unclaimed.0 >= PENDING_EVENT_CAP {
            // Evict the oldest event of the largest backlog.
            let victim = unclaimed
                .1
                .iter()
                .max_by_key(|(_, q)| q.len())
                .map(|(k, _)| k.clone());
            if let Some(k) = victim {
                let (count, backlog) = &mut *unclaimed;
                if let Some(q) = backlog.get_mut(&k) {
                    q.pop_front();
                    *count -= 1;
                    if q.is_empty() {
                        backlog.remove(&k);
                    }
                }
            }
        }
        unclaimed.0 += 1;
        unclaimed.1.entry(key).or_default().push_back(ev);
    }

    async fn close(&self) {
        self.closed.store(true, Ordering::Release);
        // Fail every waiting request and end every stream.
        self.pending.lock().await.clear();
        self.subs.lock().await.clear();
        debug!("daemon connection closed");
    }
}

fn dropped_warning(subscription: &str, dropped: u64) -> EventNotification {
    EventNotification {
        subscription: subscription.to_owned(),
        seq: 0,
        event: Event::warn(format!(
            "{dropped} event(s) dropped: client did not keep up"
        )),
    }
}

/// The terminal event must never be lost: deliver it (after a warning about
/// any dropped events) as soon as the consumer makes room. FIFO order
/// relative to already-queued events is preserved by the channel.
fn deliver_terminal(tx: mpsc::Sender<EventNotification>, dropped: u64, ev: EventNotification) {
    tokio::spawn(async move {
        if dropped > 0 {
            let _ = tx.send(dropped_warning(&ev.subscription, dropped)).await;
        }
        let _ = tx.send(ev).await;
    });
}

/// Stream of events for one subscription.
#[derive(Debug)]
pub struct EventStream {
    rx: mpsc::Receiver<EventNotification>,
}

impl EventStream {
    /// Next event, or `None` when the subscription ended.
    pub async fn next(&mut self) -> Option<EventNotification> {
        self.rx.recv().await
    }
}

impl futures::Stream for EventStream {
    type Item = EventNotification;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_api_crate() {
        assert_eq!(super::VERSION, apprentice_api::VERSION);
    }
}
