//! Local socket transport: a Windows named pipe or a Unix domain socket,
//! behind one [`Endpoint`] type that both daemon and clients understand.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use interprocess::local_socket::tokio::{Listener, RecvHalf, SendHalf, Stream, prelude::*};
use interprocess::local_socket::{GenericFilePath, GenericNamespaced, ListenerOptions, Name};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Where the daemon listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// Namespaced name (Windows: `\\.\pipe\<name>`).
    Namespaced(String),
    /// Filesystem path of a Unix domain socket.
    Path(PathBuf),
}

impl Endpoint {
    /// The platform default: a per-user named pipe on Windows, a socket file
    /// in the data directory elsewhere.
    pub fn default_for(data_dir: &Path, user: &str) -> Self {
        if cfg!(windows) {
            let digest = Sha256::digest(user.as_bytes());
            let hex = digest.iter().take(4).fold(String::new(), |mut acc, b| {
                use std::fmt::Write as _;
                let _ = write!(acc, "{b:02x}");
                acc
            });
            Endpoint::Namespaced(format!("apprentice-harness-{hex}"))
        } else {
            Endpoint::Path(data_dir.join("daemon.sock"))
        }
    }

    fn name(&self) -> io::Result<Name<'_>> {
        match self {
            Endpoint::Namespaced(n) => n.as_str().to_ns_name::<GenericNamespaced>(),
            Endpoint::Path(p) => p.as_path().to_fs_name::<GenericFilePath>(),
        }
    }

    /// Binds a listener. A stale socket file left by a crashed daemon is
    /// overwritten; the daemon lock file (M00-08) guarantees exclusivity.
    ///
    /// # Errors
    /// Returns the OS error when the name cannot be bound.
    pub fn listen(&self) -> io::Result<LocalListener> {
        let listener = ListenerOptions::new()
            .name(self.name()?)
            .try_overwrite(true)
            .create_tokio()?;
        Ok(LocalListener { inner: listener })
    }

    /// Connects to a listening daemon.
    ///
    /// # Errors
    /// Returns the OS error (typically "not found" / "connection refused"
    /// when no daemon is listening).
    pub async fn connect(&self) -> io::Result<(RecvHalf, SendHalf)> {
        let stream = Stream::connect(self.name()?).await?;
        Ok(stream.split())
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Endpoint::Namespaced(n) => write!(f, "pipe:{n}"),
            Endpoint::Path(p) => write!(f, "path:{}", p.display()),
        }
    }
}

/// Error parsing an endpoint string.
#[derive(Debug, thiserror::Error)]
#[error("invalid endpoint {0:?}; expected pipe:<name> or path:<path>")]
pub struct ParseEndpointError(String);

impl FromStr for Endpoint {
    type Err = ParseEndpointError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(n) = s.strip_prefix("pipe:") {
            if n.is_empty() {
                return Err(ParseEndpointError(s.to_owned()));
            }
            Ok(Endpoint::Namespaced(n.to_owned()))
        } else if let Some(p) = s.strip_prefix("path:") {
            if p.is_empty() {
                return Err(ParseEndpointError(s.to_owned()));
            }
            Ok(Endpoint::Path(PathBuf::from(p)))
        } else {
            Err(ParseEndpointError(s.to_owned()))
        }
    }
}

impl Serialize for Endpoint {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Endpoint {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// A bound local socket listener.
pub struct LocalListener {
    inner: Listener,
}

impl fmt::Debug for LocalListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalListener").finish_non_exhaustive()
    }
}

impl LocalListener {
    /// Waits for the next client and returns its split halves.
    ///
    /// # Errors
    /// Returns the OS error from `accept`.
    pub async fn accept(&self) -> io::Result<(RecvHalf, SendHalf)> {
        let stream = self.inner.accept().await?;
        Ok(stream.split())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_string_round_trip() {
        for e in [
            Endpoint::Namespaced("abc".into()),
            Endpoint::Path(PathBuf::from("/tmp/x.sock")),
        ] {
            let s = e.to_string();
            assert_eq!(s.parse::<Endpoint>().unwrap(), e);
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<Endpoint>(&json).unwrap(), e);
        }
        assert!("bogus".parse::<Endpoint>().is_err());
        assert!("pipe:".parse::<Endpoint>().is_err());
    }

    #[test]
    fn default_endpoint_is_per_user_and_stable() {
        let a = Endpoint::default_for(Path::new("/data"), "alice");
        let b = Endpoint::default_for(Path::new("/data"), "alice");
        let c = Endpoint::default_for(Path::new("/data"), "bob");
        assert_eq!(a, b);
        if cfg!(windows) {
            assert_ne!(a, c);
            assert!(a.to_string().starts_with("pipe:apprentice-harness-"));
        } else {
            assert_eq!(a, Endpoint::Path(PathBuf::from("/data/daemon.sock")));
        }
    }
}
