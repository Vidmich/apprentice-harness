//! `daemon.json`: how a client finds a running daemon. Written by the daemon
//! on startup, read by the CLI, GUI and `harness doctor` (task M00-08).

use std::io;
use std::path::{Path, PathBuf};

use apprentice_api::transport::Endpoint;
use serde::{Deserialize, Serialize};

/// File name inside the data directory.
pub const DAEMON_INFO_FILE: &str = "daemon.json";

/// Contents of `daemon.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub pid: u32,
    pub endpoint: Endpoint,
    /// Handshake token; never logged (the field name is redacted).
    pub token: String,
    pub api_version: u32,
    pub version: String,
    /// RFC 3339 UTC.
    pub started_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not a valid daemon.json: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

impl DaemonInfo {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(DAEMON_INFO_FILE)
    }

    /// Reads `<data_dir>/daemon.json`; `Ok(None)` when absent.
    ///
    /// # Errors
    /// Unreadable or malformed file.
    pub fn read(data_dir: &Path) -> Result<Option<Self>, DiscoveryError> {
        let path = Self::path(data_dir);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(DiscoveryError::Read { path, source }),
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|source| DiscoveryError::Parse { path, source })
    }

    /// Writes `<data_dir>/daemon.json` atomically (temp file + rename),
    /// readable by the current user only: 0600 on Unix, an ACL granting
    /// only the current user on Windows. The token inside is what
    /// authorises clients, so nobody else may read it.
    ///
    /// # Errors
    /// I/O failure, or (Windows) the ACL could not be set.
    pub fn write(&self, data_dir: &Path) -> io::Result<()> {
        let path = Self::path(data_dir);
        let tmp = data_dir.join(format!("{DAEMON_INFO_FILE}.{}.tmp", self.pid));
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let result =
            write_private(&tmp, text.as_bytes()).and_then(|()| std::fs::rename(&tmp, &path));
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    /// Removes `<data_dir>/daemon.json`; absent is fine.
    ///
    /// # Errors
    /// I/O failure other than "not found".
    pub fn remove(data_dir: &Path) -> io::Result<()> {
        match std::fs::remove_file(Self::path(data_dir)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// Creates `path` with `bytes`, readable by the current user only.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    #[cfg(unix)]
    {
        // The mode above only applies to a newly created file.
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    restrict_acl_to_current_user(path)?;
    Ok(())
}

/// Replaces the file's ACL with a single full-control entry for the current
/// user (`icacls /inheritance:r /grant:r`). Done through the `icacls` tool,
/// which ships with every Windows, rather than through the security API,
/// which needs `unsafe`.
#[cfg(windows)]
fn restrict_acl_to_current_user(path: &Path) -> io::Result<()> {
    let user = std::env::var("USERNAME")
        .ok()
        .filter(|u| !u.is_empty())
        .ok_or_else(|| io::Error::other("USERNAME is not set; cannot restrict daemon.json"))?;
    let account = match std::env::var("USERDOMAIN") {
        Ok(d) if !d.is_empty() => format!("{d}\\{user}"),
        _ => user,
    };
    let out = std::process::Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{account}:F"))
        .arg("/Q")
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "icacls failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_round_trip_and_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(DaemonInfo::read(dir.path()).unwrap().is_none());
        let info = DaemonInfo {
            pid: 42,
            endpoint: "pipe:apprentice-harness-test".parse().unwrap(),
            token: "t".into(),
            api_version: 1,
            version: "0.1.0".into(),
            started_at: "2026-09-12T00:00:00.000000Z".into(),
        };
        std::fs::write(
            DaemonInfo::path(dir.path()),
            serde_json::to_string(&info).unwrap(),
        )
        .unwrap();
        assert_eq!(DaemonInfo::read(dir.path()).unwrap(), Some(info));
        std::fs::write(DaemonInfo::path(dir.path()), "{").unwrap();
        assert!(matches!(
            DaemonInfo::read(dir.path()),
            Err(DiscoveryError::Parse { .. })
        ));
    }

    #[test]
    fn write_is_private_and_remove_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let info = DaemonInfo {
            pid: 42,
            endpoint: "pipe:apprentice-harness-test".parse().unwrap(),
            token: "t".into(),
            api_version: 1,
            version: "0.1.0".into(),
            started_at: "2026-09-12T00:00:00.000000Z".into(),
        };
        info.write(dir.path()).unwrap();
        assert_eq!(DaemonInfo::read(dir.path()).unwrap(), Some(info.clone()));
        // Overwriting an existing file works too (a restart).
        let again = DaemonInfo { pid: 43, ..info };
        again.write(dir.path()).unwrap();
        assert_eq!(DaemonInfo::read(dir.path()).unwrap().unwrap().pid, 43);
        assert!(
            std::fs::read_dir(dir.path()).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
        );

        let path = DaemonInfo::path(dir.path());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        #[cfg(windows)]
        {
            let out = std::process::Command::new("icacls")
                .arg(&path)
                .output()
                .unwrap();
            // Exactly one ACE, `<domain>\<user>:(F)`, nothing inherited.
            let text = String::from_utf8_lossy(&out.stdout);
            let user = std::env::var("USERNAME").unwrap();
            let aces: Vec<&str> = text.lines().filter(|l| l.contains(":(")).collect();
            assert_eq!(aces.len(), 1, "{text}");
            assert!(aces[0].contains(&format!("{user}:(F)")), "{text}");
            assert!(!text.contains("(I)"), "{text}");
        }

        DaemonInfo::remove(dir.path()).unwrap();
        assert!(DaemonInfo::read(dir.path()).unwrap().is_none());
        DaemonInfo::remove(dir.path()).unwrap();
    }
}
