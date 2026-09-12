//! Secrets (API keys) kept out of config files and logs.
//!
//! Lookup order is environment → persistent store (OS keychain, or a
//! user-only file on headless hosts). Values travel as [`Secret`], whose
//! `Debug` output is always `***`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

/// Secret name for a provider's API key (`anthropic` → `anthropic_api_key`).
pub fn api_key_name(provider: &str) -> String {
    format!("{provider}_api_key")
}

/// Keychain service name under which entries are stored.
pub const KEYCHAIN_SERVICE: &str = "apprentice-harness";

/// A secret string. No `Display`, `Debug` prints `***`, not `Serialize`.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value. Name the call site's intent; never log the result.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("***")
    }
}

impl From<String> for Secret {
    fn from(s: String) -> Self {
        Self(s)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("keychain: {0}")]
    Keychain(String),

    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not valid TOML: {message}")]
    Syntax { path: PathBuf, message: String },

    #[error("secret store `{0}` is read-only")]
    ReadOnly(&'static str),
}

/// Named secret storage.
pub trait SecretStore: Send + Sync {
    /// Short label reported by `auth.status`: `env`, `keychain`, `file`.
    fn label(&self) -> &'static str;

    /// # Errors
    /// Store access failed (a missing entry is `Ok(None)`).
    fn get(&self, name: &str) -> Result<Option<Secret>, SecretError>;

    /// # Errors
    /// Store access failed or the store is read-only.
    fn set(&self, name: &str, value: &Secret) -> Result<(), SecretError>;

    /// Removes the entry; removing a missing entry is not an error.
    ///
    /// # Errors
    /// Store access failed or the store is read-only.
    fn delete(&self, name: &str) -> Result<(), SecretError>;
}

// ------------------------------------------------------------------ env

/// Read-only store backed by environment variables: secret `foo_bar` is
/// read from `FOO_BAR`.
#[derive(Debug, Clone)]
pub struct EnvStore {
    vars: Option<BTreeMap<String, String>>,
}

impl EnvStore {
    /// Reads the process environment at lookup time.
    pub fn process() -> Self {
        Self { vars: None }
    }

    /// Fixed variables (tests, or callers that snapshot the environment).
    pub fn from_map<I, K, V>(vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            vars: Some(
                vars.into_iter()
                    .map(|(k, v)| (k.into(), v.into()))
                    .collect(),
            ),
        }
    }

    pub fn var_name(secret: &str) -> String {
        secret.to_ascii_uppercase()
    }
}

impl SecretStore for EnvStore {
    fn label(&self) -> &'static str {
        "env"
    }

    fn get(&self, name: &str) -> Result<Option<Secret>, SecretError> {
        let var = Self::var_name(name);
        let value = match &self.vars {
            Some(map) => map.get(&var).cloned(),
            None => std::env::var(&var).ok(),
        };
        Ok(value.filter(|v| !v.is_empty()).map(Secret))
    }

    fn set(&self, _name: &str, _value: &Secret) -> Result<(), SecretError> {
        Err(SecretError::ReadOnly("env"))
    }

    fn delete(&self, _name: &str) -> Result<(), SecretError> {
        Err(SecretError::ReadOnly("env"))
    }
}

// ------------------------------------------------------------- keychain

/// OS keychain via the `keyring` crate (Credential Manager on Windows,
/// Keychain Services on macOS, Secret Service on Linux).
#[derive(Debug, Clone)]
pub struct KeychainStore {
    service: String,
}

impl Default for KeychainStore {
    fn default() -> Self {
        Self::new(KEYCHAIN_SERVICE)
    }
}

impl KeychainStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, name: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, name).map_err(|e| SecretError::Keychain(e.to_string()))
    }
}

impl SecretStore for KeychainStore {
    fn label(&self) -> &'static str {
        "keychain"
    }

    fn get(&self, name: &str) -> Result<Option<Secret>, SecretError> {
        match self.entry(name)?.get_password() {
            Ok(v) => Ok(Some(Secret(v))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Keychain(e.to_string())),
        }
    }

    fn set(&self, name: &str, value: &Secret) -> Result<(), SecretError> {
        self.entry(name)?
            .set_password(&value.0)
            .map_err(|e| SecretError::Keychain(e.to_string()))
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        match self.entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Keychain(e.to_string())),
        }
    }
}

// ----------------------------------------------------------------- file

/// `secrets.toml` (`name = "value"` per line), created user-only. For
/// headless hosts without a keyring daemon; opt in with
/// `daemon.secret_store = "file"`.
#[derive(Debug, Clone)]
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Result<toml::Table, SecretError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(toml::Table::new()),
            Err(source) => {
                return Err(SecretError::Io {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        toml::from_str(&text).map_err(|e| SecretError::Syntax {
            path: self.path.clone(),
            message: e.message().to_owned(),
        })
    }

    fn write(&self, table: &toml::Table) -> Result<(), SecretError> {
        let io = |source| SecretError::Io {
            path: self.path.clone(),
            source,
        };
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let text = toml::to_string(table).expect("table of strings serialises");
        let tmp = self.path.with_extension("toml.tmp");
        write_user_only(&tmp, &text).map_err(io)?;
        std::fs::rename(&tmp, &self.path).map_err(io)
    }
}

#[cfg(unix)]
fn write_user_only(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(text.as_bytes())?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn write_user_only(path: &Path, text: &str) -> std::io::Result<()> {
    // The data directory under %APPDATA% is already user-scoped; a
    // per-file ACL is not applied here.
    std::fs::write(path, text)
}

impl SecretStore for FileStore {
    fn label(&self) -> &'static str {
        "file"
    }

    fn get(&self, name: &str) -> Result<Option<Secret>, SecretError> {
        Ok(self
            .read()?
            .get(name)
            .and_then(toml::Value::as_str)
            .filter(|v| !v.is_empty())
            .map(Secret::new))
    }

    fn set(&self, name: &str, value: &Secret) -> Result<(), SecretError> {
        let mut table = self.read()?;
        table.insert(name.to_owned(), toml::Value::String(value.0.clone()));
        self.write(&table)
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        let mut table = self.read()?;
        if table.remove(name).is_some() {
            self.write(&table)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- chain

/// Environment first, then the persistent store. Writes go to the
/// persistent store only.
#[derive(Clone)]
pub struct ChainStore {
    env: Arc<dyn SecretStore>,
    persistent: Arc<dyn SecretStore>,
}

impl std::fmt::Debug for ChainStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChainStore")
            .field("env", &self.env.label())
            .field("persistent", &self.persistent.label())
            .finish()
    }
}

impl ChainStore {
    pub fn new(env: Arc<dyn SecretStore>, persistent: Arc<dyn SecretStore>) -> Self {
        Self { env, persistent }
    }

    /// Finds a secret and reports which store supplied it.
    ///
    /// # Errors
    /// A store failed (a missing entry is `Ok(None)`).
    pub fn lookup(&self, name: &str) -> Result<Option<(Secret, &'static str)>, SecretError> {
        if let Some(v) = self.env.get(name)? {
            return Ok(Some((v, self.env.label())));
        }
        Ok(self
            .persistent
            .get(name)?
            .map(|v| (v, self.persistent.label())))
    }
}

impl SecretStore for ChainStore {
    fn label(&self) -> &'static str {
        self.persistent.label()
    }

    fn get(&self, name: &str) -> Result<Option<Secret>, SecretError> {
        Ok(self.lookup(name)?.map(|(v, _)| v))
    }

    fn set(&self, name: &str, value: &Secret) -> Result<(), SecretError> {
        self.persistent.set(name, value)
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        self.persistent.delete(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_never_debugs_its_value() {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Holder {
            key: Secret,
            other: u32,
        }
        let h = Holder {
            key: Secret::new("sk-ant-very-secret"),
            other: 1,
        };
        let s = format!("{h:?}");
        assert!(!s.contains("very-secret"), "{s}");
        assert!(s.contains("***"));
        assert_eq!(format!("{:?}", Some(Secret::new("x"))), "Some(***)");
    }

    #[test]
    fn env_store_maps_names_and_ignores_empty() {
        let s = EnvStore::from_map([("ANTHROPIC_API_KEY", "k1"), ("EMPTY_API_KEY", "")]);
        assert_eq!(s.get("anthropic_api_key").unwrap().unwrap().expose(), "k1");
        assert!(s.get("empty_api_key").unwrap().is_none());
        assert!(s.get("missing").unwrap().is_none());
        assert!(matches!(
            s.set("x", &Secret::new("y")),
            Err(SecretError::ReadOnly("env"))
        ));
    }

    #[test]
    fn file_store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileStore::new(dir.path().join("data").join("secrets.toml"));
        assert!(s.get("anthropic_api_key").unwrap().is_none());
        s.set("anthropic_api_key", &Secret::new("k1")).unwrap();
        s.set("other", &Secret::new("k2")).unwrap();
        assert_eq!(s.get("anthropic_api_key").unwrap().unwrap().expose(), "k1");
        s.delete("anthropic_api_key").unwrap();
        s.delete("anthropic_api_key").unwrap(); // idempotent
        assert!(s.get("anthropic_api_key").unwrap().is_none());
        assert_eq!(s.get("other").unwrap().unwrap().expose(), "k2");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(s.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn chain_prefers_env_and_writes_to_persistent() {
        let dir = tempfile::tempdir().unwrap();
        let file = Arc::new(FileStore::new(dir.path().join("secrets.toml")));
        let env = Arc::new(EnvStore::from_map([("ANTHROPIC_API_KEY", "from-env")]));
        let chain = ChainStore::new(env, file.clone());

        chain
            .set("anthropic_api_key", &Secret::new("from-file"))
            .unwrap();
        let (v, src) = chain.lookup("anthropic_api_key").unwrap().unwrap();
        assert_eq!((v.expose(), src), ("from-env", "env"));

        let chain = ChainStore::new(
            Arc::new(EnvStore::from_map::<[(&str, &str); 0], _, _>([])),
            file,
        );
        let (v, src) = chain.lookup("anthropic_api_key").unwrap().unwrap();
        assert_eq!((v.expose(), src), ("from-file", "file"));
        chain.delete("anthropic_api_key").unwrap();
        assert!(chain.lookup("anthropic_api_key").unwrap().is_none());
    }

    /// Needs a desktop session; run with `cargo test -p apprentice-core -- --ignored keychain`.
    #[test]
    #[ignore = "needs a desktop session with an OS keychain"]
    fn keychain_round_trip() {
        let s = KeychainStore::new("apprentice-harness-test");
        let name = "m00_03_test_secret";
        s.delete(name).unwrap();
        assert!(s.get(name).unwrap().is_none());
        s.set(name, &Secret::new("kc-value")).unwrap();
        assert_eq!(s.get(name).unwrap().unwrap().expose(), "kc-value");
        s.delete(name).unwrap();
        assert!(s.get(name).unwrap().is_none());
    }
}
