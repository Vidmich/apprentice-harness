//! Async front for [`TraceStore`]: a dedicated writer thread fed by a
//! bounded queue (10k). `append` resolves when the row is committed; the
//! queue back-pressures instead of dropping when the disk falls behind.
//!
//! Consecutive appends waiting in the queue are committed together (one
//! transaction, a savepoint per event), which is what keeps throughput up:
//! the per-transaction cost dominates small inserts.

use std::sync::Arc;
use std::thread::JoinHandle;

use tokio::sync::{mpsc, oneshot};

use super::EventId;
use super::error::TraceError;
use super::store::{NewEvent, TraceStore};

/// Queue capacity before `append` awaits.
pub const QUEUE_CAPACITY: usize = 10_000;
/// Most appends committed in one transaction.
pub const MAX_BATCH: usize = 256;

type Job = Box<dyn FnOnce(&TraceStore) + Send + 'static>;
type Reply = Option<oneshot::Sender<Result<EventId, TraceError>>>;

enum Cmd {
    Append(NewEvent, Reply),
    Run(Job),
    Stop,
}

pub struct TraceWriter {
    store: Arc<TraceStore>,
    tx: mpsc::Sender<Cmd>,
    thread: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for TraceWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraceWriter")
            .field("store", &self.store)
            .field("queued", &self.queued())
            .finish_non_exhaustive()
    }
}

impl TraceWriter {
    /// Starts the writer thread.
    pub fn spawn(store: Arc<TraceStore>) -> Self {
        let (tx, rx) = mpsc::channel::<Cmd>(QUEUE_CAPACITY);
        let worker = Arc::clone(&store);
        let thread = std::thread::Builder::new()
            .name("trace-writer".into())
            .spawn(move || worker_loop(&worker, rx))
            .expect("spawning the trace writer thread");
        Self {
            store,
            tx,
            thread: std::sync::Mutex::new(Some(thread)),
        }
    }

    /// The underlying store, for queries (they take the lock briefly and
    /// may run on any thread).
    pub fn store(&self) -> &Arc<TraceStore> {
        &self.store
    }

    /// Runs `f` on the writer thread, in queue order, and returns its
    /// result.
    pub async fn run<T, F>(&self, f: F) -> Result<T, TraceError>
    where
        T: Send + 'static,
        F: FnOnce(&TraceStore) -> Result<T, TraceError> + Send + 'static,
    {
        let (done, result) = oneshot::channel();
        let job: Job = Box::new(move |store| {
            let _ = done.send(f(store));
        });
        self.tx
            .send(Cmd::Run(job))
            .await
            .map_err(|_| TraceError::WriterClosed)?;
        result.await.map_err(|_| TraceError::WriterClosed)?
    }

    /// Appends an event; resolves once it is committed.
    pub async fn append(&self, ev: NewEvent) -> Result<EventId, TraceError> {
        let (done, result) = oneshot::channel();
        self.tx
            .send(Cmd::Append(ev, Some(done)))
            .await
            .map_err(|_| TraceError::WriterClosed)?;
        result.await.map_err(|_| TraceError::WriterClosed)?
    }

    /// Queues an append without waiting for it; failures are logged. When
    /// the queue is full the send is deferred to a task (back-pressure
    /// without blocking the caller).
    pub fn append_detached(&self, ev: NewEvent) {
        match self.tx.try_send(Cmd::Append(ev, None)) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(cmd)) => {
                let tx = self.tx.clone();
                match tokio::runtime::Handle::try_current() {
                    Ok(rt) => {
                        rt.spawn(async move {
                            if tx.send(cmd).await.is_err() {
                                tracing::warn!("trace writer closed; event dropped");
                            }
                        });
                    }
                    Err(_) => {
                        let _ = tx.blocking_send(cmd);
                    }
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::warn!("trace writer closed; event dropped");
            }
        }
    }

    /// Resolves when every command queued before the call has run.
    pub async fn flush(&self) -> Result<(), TraceError> {
        self.run(|_| Ok(())).await
    }

    /// Number of commands waiting in the queue.
    pub fn queued(&self) -> usize {
        QUEUE_CAPACITY.saturating_sub(self.tx.capacity())
    }

    /// Drains the queue and stops the thread. Further calls fail with
    /// [`TraceError::WriterClosed`].
    pub async fn shutdown(&self) {
        if self.tx.send(Cmd::Stop).await.is_err() {
            return; // already stopped
        }
        let handle = self
            .thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(handle) = handle {
            let _ = tokio::task::spawn_blocking(move || handle.join()).await;
        }
    }
}

fn worker_loop(store: &TraceStore, mut rx: mpsc::Receiver<Cmd>) {
    let mut next = rx.blocking_recv();
    while let Some(cmd) = next.take() {
        match cmd {
            Cmd::Stop => break,
            Cmd::Run(job) => job(store),
            Cmd::Append(ev, reply) => {
                let mut batch = vec![(ev, reply)];
                while batch.len() < MAX_BATCH {
                    match rx.try_recv() {
                        Ok(Cmd::Append(ev, reply)) => batch.push((ev, reply)),
                        Ok(other) => {
                            next = Some(other);
                            break;
                        }
                        Err(_) => break,
                    }
                }
                run_batch(store, batch);
            }
        }
        if next.is_none() {
            next = rx.blocking_recv();
        }
    }
    rx.close();
    tracing::debug!("trace writer stopped");
}

fn run_batch(store: &TraceStore, batch: Vec<(NewEvent, Reply)>) {
    let (events, replies): (Vec<_>, Vec<_>) = batch.into_iter().unzip();
    let results = if events.len() == 1 {
        let ev = events.into_iter().next().expect("one event");
        vec![store.append(ev)]
    } else {
        match store.append_batch(events) {
            Ok(results) => results,
            Err(e) => {
                let msg = e.to_string();
                let mut results = vec![Err(e)];
                results
                    .extend((1..replies.len()).map(|_| Err(TraceError::BatchFailed(msg.clone()))));
                results
            }
        }
    };
    for (reply, result) in replies.into_iter().zip(results) {
        match reply {
            Some(done) => {
                let _ = done.send(result);
            }
            None => {
                if let Err(e) = result {
                    tracing::error!(error = %e, "trace append failed");
                }
            }
        }
    }
}
