//! Single instance per data directory: an advisory lock on
//! `<data_dir>/daemon.lock`, held for the life of the process and released
//! by the OS however it ends.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use apprentice_client::DaemonInfo;
use fs4::{FileExt, TryLockError};

/// File name inside the data directory.
pub const LOCK_FILE: &str = "daemon.lock";

/// The held lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct DaemonLock {
    _file: File,
    path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another daemon holds the lock. `pid` comes from its `daemon.json`
    /// when that is readable (it is written right after the lock is
    /// taken, so a very early second start may not see it yet).
    #[error("already running (pid {})", pid.map_or_else(|| "unknown".to_owned(), |p| p.to_string()))]
    Held { pid: Option<u32> },
    #[error("cannot lock {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl DaemonLock {
    /// Takes the lock without blocking.
    ///
    /// # Errors
    /// [`LockError::Held`] when another process has it, [`LockError::Io`]
    /// when the file cannot be opened or locked.
    pub fn acquire(data_dir: &Path) -> Result<Self, LockError> {
        let path = data_dir.join(LOCK_FILE);
        let io = |source| LockError::Io {
            path: path.clone(),
            source,
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(io)?;
        // Qualified: newer std has an inherent `File::try_lock` that would
        // shadow the trait method, and the MSRV predates it.
        match FileExt::try_lock(&file) {
            Ok(()) => Ok(Self { _file: file, path }),
            Err(TryLockError::WouldBlock) => Err(LockError::Held {
                pid: DaemonInfo::read(data_dir)
                    .ok()
                    .flatten()
                    .map(|info| info.pid),
            }),
            Err(TryLockError::Error(source)) => Err(io(source)),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_in_the_same_process_sees_it_held() {
        let dir = tempfile::tempdir().unwrap();
        let first = DaemonLock::acquire(dir.path()).unwrap();
        assert!(first.path().ends_with(LOCK_FILE));
        let err = DaemonLock::acquire(dir.path()).unwrap_err();
        assert!(matches!(err, LockError::Held { pid: None }), "{err}");
        assert_eq!(err.to_string(), "already running (pid unknown)");

        // With a daemon.json the pid is named.
        DaemonInfo {
            pid: 4242,
            endpoint: "pipe:x".parse().unwrap(),
            token: "t".into(),
            api_version: 1,
            version: "0".into(),
            started_at: "2026-01-01T00:00:00.000Z".into(),
        }
        .write(dir.path())
        .unwrap();
        let err = DaemonLock::acquire(dir.path()).unwrap_err();
        assert_eq!(err.to_string(), "already running (pid 4242)");

        drop(first);
        DaemonLock::acquire(dir.path()).unwrap();
    }
}
