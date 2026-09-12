//! `daemon.json`: how a client finds a running daemon. Written by the daemon
//! on startup (task M00-08), read by the CLI, GUI and `harness doctor`.

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
}
