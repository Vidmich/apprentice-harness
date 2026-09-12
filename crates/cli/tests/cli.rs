//! Command tree, help, completions and exit codes that need no daemon
//! (task M00-09).

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

fn harness(home: &Path) -> Command {
    let mut c = Command::cargo_bin("harness").unwrap();
    c.arg("--home")
        .arg(home)
        .env_remove("HARNESS_LOG_LEVEL")
        .env_remove("HARNESS_DAEMON_PATH")
        .env("NO_COLOR", "1");
    c
}

const COMMANDS: &[&str] = &[
    "daemon",
    "config",
    "auth",
    "session",
    "run",
    "trace",
    "stats",
    "doctor",
    "completions",
];

const SUBCOMMANDS: &[&[&str]] = &[
    &["daemon", "start"],
    &["daemon", "stop"],
    &["daemon", "status"],
    &["daemon", "run"],
    &["config", "path"],
    &["config", "get"],
    &["config", "set"],
    &["auth", "set-key"],
    &["auth", "status"],
    &["session", "new"],
    &["session", "list"],
    &["run"],
    &["trace", "list"],
    &["trace", "show"],
    &["stats", "tokens"],
    &["stats", "reprice"],
    &["doctor"],
    &["completions"],
];

#[test]
fn help_lists_every_command_and_the_exit_codes() {
    let home = tempfile::tempdir().unwrap();
    let out = harness(home.path()).arg("--help").assert().success();
    let text = String::from_utf8_lossy(&out.get_output().stdout);
    for cmd in COMMANDS {
        let line = text
            .lines()
            .find(|l| l.trim_start().starts_with(&format!("{cmd} ")))
            .unwrap_or_else(|| panic!("`{cmd}` missing from --help:\n{text}"));
        // One-line description after the name.
        assert!(line.trim().len() > cmd.len() + 4, "{line}");
    }
    assert!(text.contains("130  interrupted"), "{text}");
    assert!(text.contains("Usage: harness "), "{text}");

    for path in SUBCOMMANDS {
        let out = harness(home.path())
            .args(*path)
            .arg("--help")
            .assert()
            .success();
        let text = String::from_utf8_lossy(&out.get_output().stdout);
        assert!(text.contains("Usage:"), "{path:?}: {text}");
    }
    // Examples where they matter.
    harness(home.path())
        .args(["run", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"))
        .stdout(predicate::str::contains("--no-apprentice"));
    harness(home.path())
        .args(["config", "set", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("harness config set mentor.model"));
}

#[test]
fn no_command_shows_help_and_bad_arguments_exit_1() {
    let home = tempfile::tempdir().unwrap();
    harness(home.path())
        .assert()
        .code(1)
        .stderr(predicate::str::contains("Usage:"));
    harness(home.path())
        .args(["session", "list", "--bogus"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("--bogus"));
    harness(home.path())
        .args(["run", "hi", "--session", "s", "--workspace", "."])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("cannot be used with"));
    harness(home.path()).args(["nope"]).assert().code(1);
    harness(home.path())
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("harness "));
}

#[test]
fn completions_render_for_every_shell() {
    let home = tempfile::tempdir().unwrap();
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let out = harness(home.path())
            .args(["completions", shell])
            .assert()
            .success();
        let text = String::from_utf8_lossy(&out.get_output().stdout);
        assert!(text.len() > 500, "{shell}: {text}");
        assert!(text.contains("harness"), "{shell}");
        assert!(text.contains("session"), "{shell}");
    }
    harness(home.path())
        .args(["completions", "tcsh"])
        .assert()
        .code(1);
}

#[cfg(unix)]
#[test]
fn bash_completions_load() {
    let home = tempfile::tempdir().unwrap();
    let out = harness(home.path())
        .args(["completions", "bash"])
        .assert()
        .success();
    let script = home.path().join("harness.bash");
    std::fs::write(&script, &out.get_output().stdout).unwrap();
    let status = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {} && complete -p harness",
            script.display()
        ))
        .status();
    if let Ok(status) = status {
        assert!(status.success());
    }
}

#[cfg(windows)]
#[test]
fn powershell_completions_load() {
    let home = tempfile::tempdir().unwrap();
    let out = harness(home.path())
        .args(["completions", "powershell"])
        .assert()
        .success();
    let script = home.path().join("harness.ps1");
    std::fs::write(&script, &out.get_output().stdout).unwrap();
    let pwsh = ["pwsh", "powershell"]
        .into_iter()
        .find(|p| which(p))
        .expect("a PowerShell");
    let status = std::process::Command::new(pwsh)
        .args(["-NoProfile", "-NonInteractive", "-File"])
        .arg(&script)
        .status()
        .unwrap();
    assert!(status.success());
}

#[cfg(windows)]
fn which(program: &str) -> bool {
    std::process::Command::new(program)
        .args(["-NoProfile", "-Command", "exit 0"])
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn without_a_daemon_no_spawn_exits_3_and_status_exits_3() {
    let home = tempfile::tempdir().unwrap();
    harness(home.path())
        .args(["--no-spawn", "session", "list"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("no daemon is running"))
        .stderr(predicate::str::contains("harness daemon start"));
    // `--json` still prints exactly one document, on stdout.
    let out = harness(home.path())
        .args(["--no-spawn", "--json", "session", "list"])
        .assert()
        .code(3);
    let doc: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert!(
        doc["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no daemon")
    );
    // `status` and `stop` never spawn.
    harness(home.path())
        .args(["daemon", "status"])
        .assert()
        .code(3);
    harness(home.path())
        .args(["daemon", "stop"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no daemon is running"));
    // A stale daemon.json is the same as none.
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        data.join("daemon.json"),
        serde_json::json!({
            "pid": 999_999,
            "endpoint": "pipe:apprentice-harness-cli-stale-test",
            "token": "t",
            "api_version": 1,
            "version": "0.0.1",
            "started_at": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();
    harness(home.path())
        .args(["--no-spawn", "trace", "list"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("stale"));
}

#[test]
fn spawning_without_a_daemon_binary_exits_3() {
    let home = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    // No harnessd anywhere: not next to the binary (a copy elsewhere), not
    // in HARNESS_DAEMON_PATH, not on PATH.
    let copy = empty
        .path()
        .join(format!("harness{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(assert_cmd::cargo::cargo_bin("harness"), &copy).unwrap();
    let mut c = Command::new(copy);
    c.arg("--home")
        .arg(home.path())
        .env("PATH", empty.path())
        .env_remove("HARNESS_DAEMON_PATH")
        .env_remove("HARNESS_LOG_LEVEL")
        .args(["session", "list"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "cannot find the `harnessd` binary",
        ));
}
