//! Background jobs: commands started with `background: true`, pumped
//! by a task of their own and looked after by `shell_jobs`.
//!
//! One [`Jobs`] registry lives for the daemon (the two shell tools
//! share it). A job's output is kept in memory under the same caps as
//! a foreground run's; finished jobs stay listed until
//! [`KEEP_FINISHED`] newer ones have finished.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::capture::Transcript;
use super::process::{Outcome, Spawned, pump};
use crate::trace::AgentId;

/// Finished jobs remembered per daemon.
const KEEP_FINISHED: usize = 50;

/// Everything that is known about a job while it runs and after.
#[derive(Debug)]
pub(super) struct State {
    pub transcript: Transcript,
    pub outcome: Option<Outcome>,
}

#[derive(Debug)]
pub(super) struct Job {
    pub id: String,
    pub command: String,
    pub description: Option<String>,
    pub pid: u32,
    pub detached: bool,
    pub agent_id: AgentId,
    pub started: Instant,
    /// The run's `timeout_s`, for its footer.
    pub timeout: Duration,
    state: Mutex<State>,
    kill: CancellationToken,
    done: watch::Sender<bool>,
}

impl Job {
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn is_running(&self) -> bool {
        self.state().outcome.is_none()
    }

    /// Asks the pump to kill the tree; the outcome follows shortly.
    pub fn kill(&self) {
        self.kill.cancel();
    }

    /// Waits for the job to end, at most `timeout`; `true` when it did.
    pub async fn wait(&self, timeout: Duration) -> bool {
        let mut done = self.done.subscribe();
        tokio::time::timeout(timeout, done.wait_for(|d| *d))
            .await
            .is_ok_and(|r| r.is_ok())
    }
}

/// What [`Jobs::start`] needs to know about a new job.
#[derive(Debug)]
pub(super) struct NewJob {
    pub command: String,
    pub description: Option<String>,
    pub detached: bool,
    pub agent_id: AgentId,
    pub timeout: Duration,
    /// Killing the job when this fires (the agent's token) — unless
    /// detached, when only `shell_jobs kill` can.
    pub cancel: CancellationToken,
    pub max_bytes: usize,
}

/// The daemon's jobs.
#[derive(Debug, Default)]
pub(super) struct Jobs {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    next: u64,
    /// In start order.
    jobs: Vec<Arc<Job>>,
}

impl Jobs {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Registers `spawned` as a job and starts pumping its output.
    pub fn start(&self, spawned: Spawned, new: NewJob) -> Arc<Job> {
        let kill = if new.detached {
            CancellationToken::new()
        } else {
            new.cancel
        };
        let (done, _) = watch::channel(false);
        let job = {
            let mut inner = self.lock();
            inner.next += 1;
            let job = Arc::new(Job {
                id: format!("j{}", inner.next),
                command: new.command,
                description: new.description,
                pid: spawned.pid,
                detached: new.detached,
                agent_id: new.agent_id,
                started: Instant::now(),
                timeout: new.timeout,
                state: Mutex::new(State {
                    transcript: Transcript::new(new.max_bytes),
                    outcome: None,
                }),
                kill,
                done,
            });
            inner.jobs.push(Arc::clone(&job));
            prune(&mut inner.jobs);
            job
        };
        let pumped = Arc::clone(&job);
        tokio::spawn(async move {
            let outcome = pump(spawned, new.timeout, &pumped.kill, |stream, bytes| {
                pumped.state().transcript.push(stream, bytes);
            })
            .await;
            pumped.state().outcome = Some(outcome);
            pumped.done.send_replace(true);
        });
        job
    }

    pub fn get(&self, id: &str) -> Option<Arc<Job>> {
        self.lock().jobs.iter().find(|j| j.id == id).cloned()
    }

    /// Every job, oldest first.
    pub fn list(&self) -> Vec<Arc<Job>> {
        self.lock().jobs.clone()
    }
}

/// Drops the oldest finished jobs beyond [`KEEP_FINISHED`].
fn prune(jobs: &mut Vec<Arc<Job>>) {
    let finished = jobs.iter().filter(|j| !j.is_running()).count();
    let mut excess = finished.saturating_sub(KEEP_FINISHED);
    jobs.retain(|j| {
        if excess > 0 && !j.is_running() {
            excess -= 1;
            false
        } else {
            true
        }
    });
}
