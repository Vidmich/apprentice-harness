//! Spawning a command and pumping its output.
//!
//! The child is put in a process group (Unix) or a Job Object
//! (Windows) at spawn, so a timeout, a cancellation or a `kill` takes
//! the whole tree down at once — a test runner's forgotten children
//! included. Stdout and stderr are read concurrently in 64 KiB chunks,
//! cut at line ends, and handed to the caller as they arrive; the pump
//! ends when the child has exited *and* both pipes are closed, killing
//! what still holds them open after a short grace.

use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt as _;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::program::{Program, environment};
use crate::config::ShellConfig;
use crate::tools::ProgressStream;

/// Read size per pipe.
const CHUNK: usize = 64 * 1024;
/// How long the pump waits for the pipes to close after the child
/// exited before it kills whatever still holds them.
const PIPE_GRACE: Duration = Duration::from_millis(500);
/// How long the pump waits for the pipes to close after killing the
/// tree before it gives up on them.
const KILL_GRACE: Duration = Duration::from_secs(2);

/// A spawned command, its tree handle and the pipes.
#[derive(Debug)]
pub(super) struct Spawned {
    pub child: Child,
    pub tree: Tree,
    pub pid: u32,
}

/// Spawns `command` through `program` in `cwd`.
///
/// # Errors
/// The program cannot be started.
pub(super) fn spawn(
    program: &Program,
    config: &ShellConfig,
    command: &str,
    cwd: &Path,
    kill_on_drop: bool,
) -> std::io::Result<Spawned> {
    let mut cmd = Command::new(&program.path);
    cmd.args(program.args_for(command))
        .current_dir(cwd)
        .env_clear()
        .envs(environment(config))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(kill_on_drop);
    platform::prepare(&mut cmd);
    let child = cmd.spawn()?;
    let pid = child.id().unwrap_or(0);
    let tree = Tree::new(&child, kill_on_drop);
    Ok(Spawned { child, tree, pid })
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ended {
    Exited(ExitStatus),
    TimedOut,
    /// The cancel token fired (agent cancelled, or `shell_jobs kill`).
    Killed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Outcome {
    pub ended: Ended,
    pub duration: Duration,
    /// The child exited but something kept its pipes open past the
    /// grace, and was killed for it.
    pub orphans_killed: bool,
}

/// Runs the pump: feeds every chunk to `sink` (whole lines when the
/// stream provides them) until the child is done, `timeout` passes or
/// `cancel` fires — the latter two kill the tree first.
pub(super) async fn pump(
    spawned: Spawned,
    timeout: Duration,
    cancel: &CancellationToken,
    mut sink: impl FnMut(ProgressStream, &[u8]) + Send,
) -> Outcome {
    let started = Instant::now();
    let Spawned {
        mut child,
        mut tree,
        ..
    } = spawned;
    let (tx, mut rx) = mpsc::channel::<(ProgressStream, Vec<u8>)>(32);
    let readers = [
        child
            .stdout
            .take()
            .map(|r| tokio::spawn(read_stream(r, ProgressStream::Stdout, tx.clone()))),
        child
            .stderr
            .take()
            .map(|r| tokio::spawn(read_stream(r, ProgressStream::Stderr, tx.clone()))),
    ];
    drop(tx);

    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    // Re-armed as needed; never meant to fire as is.
    let grace = tokio::time::sleep(Duration::from_secs(10 * 365 * 86_400));
    tokio::pin!(grace);
    let mut grace_armed = false;
    let mut status: Option<ExitStatus> = None;
    let mut ended: Option<Ended> = None;
    let mut orphans_killed = false;
    let mut pipes_open = true;

    while pipes_open {
        tokio::select! {
            biased;
            () = cancel.cancelled(), if ended.is_none() => {
                ended = Some(Ended::Killed);
                tree.kill();
                grace.as_mut().reset(tokio::time::Instant::now() + KILL_GRACE);
                grace_armed = true;
            }
            () = &mut deadline, if ended.is_none() => {
                ended = Some(Ended::TimedOut);
                tree.kill();
                grace.as_mut().reset(tokio::time::Instant::now() + KILL_GRACE);
                grace_armed = true;
            }
            s = child.wait(), if status.is_none() => {
                status = Some(s.unwrap_or_default());
                if ended.is_none() {
                    // The pipes usually close right behind the exit;
                    // give stragglers a moment, then take them down.
                    grace.as_mut().reset(tokio::time::Instant::now() + PIPE_GRACE);
                    grace_armed = true;
                }
            }
            () = &mut grace, if grace_armed => {
                grace_armed = false;
                if ended.is_none() {
                    // Exited, pipes still open: children of the child.
                    orphans_killed = true;
                    tree.kill();
                    grace.as_mut().reset(tokio::time::Instant::now() + KILL_GRACE);
                    grace_armed = true;
                } else {
                    // Killed, and still something holds the pipes.
                    warn!("shell: output pipes still open 2 s after the kill; abandoning them");
                    pipes_open = false;
                }
            }
            chunk = rx.recv() => match chunk {
                Some((stream, bytes)) => sink(stream, &bytes),
                None => pipes_open = false,
            }
        }
    }
    for reader in readers.into_iter().flatten() {
        reader.abort();
    }
    // The child itself, if it is somehow still there.
    if status.is_none() {
        tree.kill();
        status = tokio::time::timeout(KILL_GRACE, child.wait())
            .await
            .ok()
            .and_then(Result::ok);
    }
    let ended = ended.unwrap_or_else(|| Ended::Exited(status.unwrap_or_default()));
    debug!(?ended, orphans_killed, "shell: command ended");
    Outcome {
        ended,
        duration: started.elapsed(),
        orphans_killed,
    }
}

/// Reads one pipe to EOF, sending complete lines (or a full chunk, or
/// the final partial line).
async fn read_stream<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    stream: ProgressStream,
    tx: mpsc::Sender<(ProgressStream, Vec<u8>)>,
) {
    let mut buf = vec![0u8; CHUNK];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                pending.extend_from_slice(&buf[..n]);
                let cut = match pending.iter().rposition(|b| *b == b'\n') {
                    Some(i) => i + 1,
                    None if pending.len() >= CHUNK => pending.len(),
                    None => continue,
                };
                let batch = pending.drain(..cut).collect();
                if tx.send((stream, batch)).await.is_err() {
                    return;
                }
            }
        }
    }
    if !pending.is_empty() {
        let _ = tx.send((stream, pending)).await;
    }
}

/// The handle on the command's process tree. Killing is idempotent;
/// dropping it kills the tree unless the handle was made for a
/// detached job.
#[derive(Debug)]
pub(super) struct Tree {
    inner: platform::Handle,
    kill_on_drop: bool,
    killed: bool,
}

impl Tree {
    fn new(child: &Child, kill_on_drop: bool) -> Self {
        Self {
            inner: platform::Handle::new(child, kill_on_drop),
            kill_on_drop,
            killed: false,
        }
    }

    /// Kills every process in the tree (SIGKILL / `TerminateJobObject`).
    pub fn kill(&mut self) {
        if !self.killed {
            self.killed = true;
            self.inner.kill();
        }
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        if self.kill_on_drop {
            self.kill();
        }
    }
}

#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "Job Objects have no safe wrapper; every call is on handles this module owns"
)]
mod platform {
    use std::os::windows::io::RawHandle;

    use tokio::process::{Child, Command};
    use tracing::warn;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    /// No console window for the shell when the daemon has none.
    pub fn prepare(cmd: &mut Command) {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    /// A Job Object the child was assigned to (`None` when that failed;
    /// then only the direct child can be killed).
    #[derive(Debug)]
    pub struct Handle {
        job: Option<Job>,
        process: RawHandle,
    }

    // SAFETY: both are kernel object references (the process handle is
    // owned by the `Child`, which outlives this value); Win32 allows
    // using and closing them from any thread.
    unsafe impl Send for Handle {}
    unsafe impl Sync for Handle {}

    #[derive(Debug)]
    struct Job(HANDLE);

    impl Job {
        fn create(kill_on_close: bool) -> std::io::Result<Self> {
            // SAFETY: plain Win32 calls with valid arguments; the handle
            // is owned by the returned value and closed in `Drop`.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return Err(std::io::Error::last_os_error());
                }
                let job = Self(job);
                if kill_on_close {
                    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                    let ok = SetInformationJobObject(
                        job.0,
                        JobObjectExtendedLimitInformation,
                        std::ptr::from_ref(&info).cast(),
                        u32::try_from(std::mem::size_of_val(&info)).unwrap_or(u32::MAX),
                    );
                    if ok == 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(job)
            }
        }

        fn assign(&self, process: RawHandle) -> std::io::Result<()> {
            // SAFETY: both handles are valid for the duration of the call.
            let ok = unsafe { AssignProcessToJobObject(self.0, process) };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }

        fn terminate(&self) {
            // SAFETY: the job handle is valid until `Drop`.
            unsafe { TerminateJobObject(self.0, 1) };
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: closes the handle this value owns, once.
            unsafe { CloseHandle(self.0) };
        }
    }

    impl Handle {
        pub fn new(child: &Child, kill_on_drop: bool) -> Self {
            let process = child.raw_handle().unwrap_or(std::ptr::null_mut());
            let job = Job::create(kill_on_drop)
                .and_then(|job| job.assign(process).map(|()| job))
                .map_err(|e| warn!(error = %e, "shell: cannot put the command in a job object"))
                .ok();
            Self { job, process }
        }

        pub fn kill(&mut self) {
            match &self.job {
                Some(job) => job.terminate(),
                None => {
                    // SAFETY: `process` is the child's handle, owned by
                    // the `Child` for as long as this `Handle` lives.
                    unsafe {
                        windows_sys::Win32::System::Threading::TerminateProcess(self.process, 1);
                    }
                }
            }
        }
    }
}

#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "killpg has no safe wrapper; the group id is the child's own pid"
)]
mod platform {
    use tokio::process::{Child, Command};

    /// A process group of its own, so the whole tree can be signalled.
    pub fn prepare(cmd: &mut Command) {
        cmd.process_group(0);
    }

    #[derive(Debug)]
    pub struct Handle {
        pgid: Option<i32>,
    }

    impl Handle {
        pub fn new(child: &Child, _kill_on_drop: bool) -> Self {
            Self {
                pgid: child.id().and_then(|id| i32::try_from(id).ok()),
            }
        }

        pub fn kill(&mut self) {
            if let Some(pgid) = self.pgid {
                // SAFETY: a signal to a process group id; nothing is
                // dereferenced. A stale id is answered with ESRCH.
                unsafe {
                    libc::killpg(pgid, libc::SIGKILL);
                }
            }
        }
    }
}
