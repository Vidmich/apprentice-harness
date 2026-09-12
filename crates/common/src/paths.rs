//! Where configuration and data live on disk.

use std::path::{Path, PathBuf};

/// Environment variable that puts config and data under one directory.
pub const HOME_ENV: &str = "HARNESS_HOME";

/// Application name used for the platform directories.
pub const APP_NAME: &str = "apprentice-harness";

/// Resolved locations for this user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Holds `config.toml`.
    pub config_dir: PathBuf,
    /// Holds `traces.sqlite`, `blobs/`, `logs/`, `models/`, `daemon.json`,
    /// `daemon.lock`, `secrets.toml`.
    pub data_dir: PathBuf,
}

impl Paths {
    /// Uses `HARNESS_HOME` from the process environment when set, otherwise
    /// the platform directories.
    ///
    /// # Errors
    /// Fails only when the platform provides no home directory at all.
    pub fn discover() -> Result<Self, NoHomeDir> {
        match std::env::var_os(HOME_ENV) {
            Some(home) if !home.is_empty() => Ok(Self::from_home(home)),
            _ => Self::platform(),
        }
    }

    /// Everything under one directory (the `HARNESS_HOME` layout).
    pub fn from_home(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self {
            config_dir: home.clone(),
            data_dir: home.join("data"),
        }
    }

    /// Platform directories via `directories::ProjectDirs`.
    ///
    /// # Errors
    /// Fails when no home directory can be determined.
    pub fn platform() -> Result<Self, NoHomeDir> {
        let dirs = directories::ProjectDirs::from("", "", APP_NAME).ok_or(NoHomeDir)?;
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
        })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn secrets_file(&self) -> PathBuf {
        self.data_dir.join("secrets.toml")
    }

    /// Workspace-level config file for a workspace root.
    pub fn workspace_config_file(workspace: &Path) -> PathBuf {
        workspace.join(".harness").join("config.toml")
    }
}

/// The platform reports no home directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("cannot determine a home directory for configuration; set {HOME_ENV}")]
pub struct NoHomeDir;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_layout_nests_data_under_home() {
        let p = Paths::from_home("/x/harness");
        assert_eq!(p.config_file(), Path::new("/x/harness/config.toml"));
        assert_eq!(p.data_dir, Path::new("/x/harness/data"));
        assert_eq!(p.secrets_file(), Path::new("/x/harness/data/secrets.toml"));
    }

    #[test]
    fn workspace_file_is_under_dot_harness() {
        assert_eq!(
            Paths::workspace_config_file(Path::new("/repo")),
            Path::new("/repo/.harness/config.toml")
        );
    }
}
