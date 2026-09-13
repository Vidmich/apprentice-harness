//! Which program runs a command, with what arguments, in what
//! environment.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::config::ShellConfig;

/// Appended to a PowerShell command so the process exits with the
/// command's own status: `-Command` alone turns every failure into 1.
/// `$?` is that of the last statement; a failed native command leaves
/// its code in `$LASTEXITCODE`.
const PWSH_TRAILER: &str = "\n$__ok = $?\nif (-not $__ok) { if ($LASTEXITCODE -is [int] -and $LASTEXITCODE -ne 0) { exit $LASTEXITCODE } else { exit 1 } }\nexit 0\n";

/// The shell a command runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Program {
    pub path: PathBuf,
    /// Arguments before the command.
    pub args: Vec<String>,
    /// PowerShell: the command gets [`PWSH_TRAILER`].
    pub powershell: bool,
    /// `pwsh`, `bash`, ... for the metadata.
    pub name: String,
}

impl Program {
    /// The configured program, or the platform default.
    pub fn resolve(config: &ShellConfig) -> Self {
        if config.program.is_empty() {
            return Self::default_for_platform();
        }
        let path = PathBuf::from(&config.program);
        let name = stem(&path);
        let powershell = is_powershell(&name);
        let args = if config.args.is_empty() && powershell {
            pwsh_args()
        } else {
            config.args.clone()
        };
        Self {
            path,
            args,
            powershell,
            name,
        }
    }

    fn default_for_platform() -> Self {
        static DEFAULT: OnceLock<Program> = OnceLock::new();
        DEFAULT.get_or_init(Self::detect).clone()
    }

    #[cfg(windows)]
    fn detect() -> Self {
        let path = find_in_path("pwsh.exe")
            .or_else(|| find_in_path("powershell.exe"))
            .unwrap_or_else(|| PathBuf::from("powershell.exe"));
        Self {
            name: stem(&path),
            path,
            args: pwsh_args(),
            powershell: true,
        }
    }

    #[cfg(not(windows))]
    fn detect() -> Self {
        let path = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && p.is_file())
            .unwrap_or_else(|| PathBuf::from("/bin/sh"));
        Self {
            name: stem(&path),
            path,
            args: vec!["-lc".to_owned()],
            powershell: false,
        }
    }

    /// The argument list for `command`.
    pub fn args_for(&self, command: &str) -> Vec<String> {
        let mut args = self.args.clone();
        if self.powershell {
            let mut script = String::with_capacity(command.len() + PWSH_TRAILER.len());
            script.push_str(command);
            script.push_str(PWSH_TRAILER);
            args.push(script);
        } else {
            args.push(command.to_owned());
        }
        args
    }
}

fn pwsh_args() -> Vec<String> {
    ["-NoProfile", "-NonInteractive", "-Command"]
        .map(str::to_owned)
        .to_vec()
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn is_powershell(name: &str) -> bool {
    name.eq_ignore_ascii_case("pwsh") || name.eq_ignore_ascii_case("powershell")
}

/// The first `PATH` entry that has `file`.
#[cfg(windows)]
fn find_in_path(file: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(file))
        .find(|p| p.is_file())
}

/// The command's environment: the daemon's minus `scrub_env`, plus the
/// harness hints and `config.env`.
pub(super) fn environment(config: &ShellConfig) -> Vec<(OsString, OsString)> {
    let patterns: Vec<&str> = config.scrub_env.iter().map(String::as_str).collect();
    let mut env: BTreeMap<OsString, OsString> = std::env::vars_os()
        .filter(|(name, _)| !scrubbed(name, &patterns))
        .collect();
    for (name, value) in [("HARNESS", "1"), ("NO_COLOR", "1"), ("TERM", "dumb")] {
        env.insert(name.into(), value.into());
    }
    for (name, value) in &config.env {
        env.insert(name.into(), value.into());
    }
    env.into_iter().collect()
}

/// `true` when `name` matches one of `patterns` (`*` matches any run of
/// characters; case-insensitive).
pub(super) fn scrubbed(name: &OsStr, patterns: &[&str]) -> bool {
    let name = name.to_string_lossy();
    patterns
        .iter()
        .any(|p| glob_match(&p.to_ascii_uppercase(), &name.to_ascii_uppercase()))
}

fn glob_match(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((head, rest)) => {
            let Some(after) = text.strip_prefix(head) else {
                return false;
            };
            if rest.is_empty() {
                return true;
            }
            (0..=after.len())
                .filter(|i| after.is_char_boundary(*i))
                .any(|i| glob_match(rest, &after[i..]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_match_names_case_insensitively() {
        let p = &["ANTHROPIC_API_KEY", "*_TOKEN", "AWS_*", "*_SECRET_*"];
        for name in [
            "ANTHROPIC_API_KEY",
            "anthropic_api_key",
            "GITHUB_TOKEN",
            "AWS_ACCESS_KEY_ID",
            "AWS_",
            "MY_SECRET_THING",
        ] {
            assert!(scrubbed(OsStr::new(name), p), "{name}");
        }
        for name in ["PATH", "TOKEN", "XAWS_Y", "AWS", "MY_SECRET"] {
            assert!(!scrubbed(OsStr::new(name), p), "{name}");
        }
    }

    #[test]
    fn environment_scrubs_and_adds_hints() {
        // `CARGO_PKG_NAME` is set by cargo for every test binary.
        let config = ShellConfig {
            scrub_env: vec!["CARGO_PKG_*".into()],
            env: BTreeMap::from([("CI".to_owned(), "1".to_owned())]),
            ..ShellConfig::default()
        };
        let env = environment(&config);
        let get = |k: &str| {
            env.iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        assert_eq!(get("CARGO_PKG_NAME"), None);
        assert_eq!(get("HARNESS").as_deref(), Some("1"));
        assert_eq!(get("NO_COLOR").as_deref(), Some("1"));
        assert_eq!(get("TERM").as_deref(), Some("dumb"));
        assert_eq!(get("CI").as_deref(), Some("1"));
        assert!(get("PATH").is_some() || get("Path").is_some());
    }

    #[test]
    fn configured_program_is_used_as_is() {
        let custom = Program::resolve(&ShellConfig {
            program: "/usr/bin/bash".into(),
            args: vec!["-c".into()],
            ..ShellConfig::default()
        });
        assert_eq!(custom.name, "bash");
        assert!(!custom.powershell);
        assert_eq!(custom.args_for("echo hi"), ["-c", "echo hi"]);

        let ps = Program::resolve(&ShellConfig {
            program: r"C:\Program Files\PowerShell\7\pwsh.exe".into(),
            ..ShellConfig::default()
        });
        assert!(ps.powershell);
        let args = ps.args_for("echo hi");
        assert_eq!(&args[..3], ["-NoProfile", "-NonInteractive", "-Command"]);
        assert!(args[3].starts_with("echo hi\n"));
        assert!(args[3].ends_with("exit 0\n"));

        let default = Program::resolve(&ShellConfig::default());
        assert_eq!(default.powershell, cfg!(windows));
        assert!(!default.name.is_empty());
    }
}
