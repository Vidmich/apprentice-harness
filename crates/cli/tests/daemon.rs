//! The CLI against a real `harnessd` (task M00-09): spawning on first
//! use, `daemon start|status|stop`, `auth set-key` / `status` without the
//! key ever showing, sessions, config and traces. The daemon binary is
//! found next to `harness`, or built with `cargo build -p harnessd` when
//! missing.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use apprentice_client::DaemonInfo;
use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

fn harnessd() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let harness = assert_cmd::cargo::cargo_bin("harness");
        let bin = harness
            .parent()
            .unwrap()
            .join(format!("harnessd{}", std::env::consts::EXE_SUFFIX));
        if !bin.is_file() {
            let status = std::process::Command::new(env!("CARGO"))
                .args(["build", "-p", "harnessd", "--bin", "harnessd"])
                .status()
                .expect("cargo build");
            assert!(status.success(), "building harnessd failed");
        }
        assert!(bin.is_file(), "{} missing", bin.display());
        bin
    })
}

fn harness(home: &Path) -> Command {
    let mut c = Command::cargo_bin("harness").unwrap();
    c.arg("--home")
        .arg(home)
        .env_remove("HARNESS_LOG_LEVEL")
        .env_remove("ANTHROPIC_API_KEY")
        .env("HARNESS_DAEMON_PATH", harnessd())
        .env("NO_COLOR", "1")
        .timeout(Duration::from_secs(30));
    c
}

fn json(out: &assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&out.get_output().stdout).unwrap()
}

/// A home whose daemon keeps secrets in a file, so the test never touches
/// the real keychain.
fn home_with_file_secrets() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[daemon]\nsecret_store = \"file\"\n",
    )
    .unwrap();
    home
}

fn stop(home: &Path) {
    harness(home).args(["daemon", "stop"]).assert().success();
    let data = home.join("data");
    let deadline = Instant::now() + Duration::from_secs(10);
    while DaemonInfo::read(&data).unwrap().is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn first_use_spawns_the_daemon_and_every_command_works_against_it() {
    let home = home_with_file_secrets();
    let h = home.path();
    let data = h.join("data");
    let result = std::panic::catch_unwind(|| {
        // `session new` starts the daemon (and says so on stderr).
        let out = harness(h)
            .args(["session", "new", "--title", "first", "--workspace", "."])
            .assert()
            .success()
            .stderr(predicate::str::contains("started daemon (pid "));
        let session_id = String::from_utf8_lossy(&out.get_output().stdout)
            .trim()
            .to_owned();
        assert!(session_id.len() > 20, "{session_id}");
        let info = DaemonInfo::read(&data).unwrap().expect("daemon.json");

        // The second client reuses it.
        harness(h)
            .args(["daemon", "start"])
            .assert()
            .success()
            .stdout(predicate::str::contains(format!(
                "daemon already running (pid {}",
                info.pid
            )));
        let status = json(
            &harness(h)
                .args(["--json", "daemon", "status"])
                .assert()
                .success(),
        );
        assert_eq!(status["pid"], info.pid);
        assert_eq!(status["sessions_open"], 1);
        harness(h)
            .args(["daemon", "status"])
            .assert()
            .success()
            .stdout(predicate::str::contains(format!(
                "pid          {}\n",
                info.pid
            )))
            .stdout(predicate::str::contains("endpoint     "));

        // Sessions and traces.
        harness(h)
            .args(["session", "list"])
            .assert()
            .success()
            .stdout(predicate::str::contains(&session_id))
            .stdout(predicate::str::contains("first"));
        let list = json(
            &harness(h)
                .args(["--json", "s", "list", "--limit", "5"])
                .assert()
                .success(),
        );
        assert_eq!(list["sessions"][0]["id"], session_id);
        let traces = json(
            &harness(h)
                .args(["--json", "trace", "list", "--kind", "session.created"])
                .assert()
                .success(),
        );
        let event_id = traces["events"][0]["id"].as_str().unwrap().to_owned();
        assert_eq!(traces["events"][0]["session_id"], session_id);
        harness(h)
            .args(["trace", "show", &event_id])
            .assert()
            .success()
            .stdout(predicate::str::contains("kind     session.created"))
            .stdout(predicate::str::contains("\"title\": \"first\""));
        harness(h)
            .args(["trace", "show", "no-such-event"])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("[not_found]"));

        // Config round trip.
        harness(h)
            .args(["config", "set", "mentor.model", "claude-test"])
            .assert()
            .success();
        harness(h)
            .args(["config", "get", "mentor.model"])
            .assert()
            .success()
            .stdout("claude-test\n")
            .stderr(predicate::str::contains("# from user"));
        let got = json(
            &harness(h)
                .args(["--json", "config", "get", "mentor.model"])
                .assert()
                .success(),
        );
        assert_eq!(got["value"], "claude-test");
        assert_eq!(got["source"], "user");
        harness(h)
            .args(["config", "get"])
            .assert()
            .success()
            .stdout(predicate::str::contains("[mentor]"))
            .stdout(predicate::str::contains("model = \"claude-test\""));
        harness(h)
            .args(["config", "set", "mentor.model", "null"])
            .assert()
            .success()
            .stderr(predicate::str::contains("removed mentor.model"));
        harness(h)
            .args(["config", "set", "mentor.nope", "1"])
            .assert()
            .code(2)
            .stderr(predicate::str::starts_with("error: "));
        let paths = json(
            &harness(h)
                .args(["--json", "config", "path"])
                .assert()
                .success(),
        );
        assert_eq!(paths["data_dir"], data.display().to_string());

        // Auth: the key goes in over stdin and never comes back out.
        harness(h)
            .args(["auth", "status"])
            .assert()
            .success()
            .stdout(predicate::str::contains("anthropic  not configured"));
        let out = harness(h)
            .args(["--json", "auth", "set-key", "--stdin"])
            .write_stdin("sk-ant-test-secret-value\n")
            .assert()
            .success();
        let doc = json(&out);
        assert_eq!(doc["provider"], "anthropic");
        assert_eq!(doc["configured"], true);
        assert_eq!(doc["source"], "file");
        let out = harness(h)
            .args(["--json", "auth", "status"])
            .assert()
            .success();
        let doc = json(&out);
        assert_eq!(doc["providers"][0]["configured"], true);
        assert!(!String::from_utf8_lossy(&out.get_output().stdout).contains("sk-ant"));
        harness(h)
            .args(["auth", "status"])
            .assert()
            .success()
            .stdout(predicate::str::contains("anthropic  configured  file"))
            .stdout(predicate::str::contains("sk-ant").not());
        harness(h)
            .args(["auth", "set-key", "--stdin"])
            .write_stdin("\n")
            .assert()
            .code(1)
            .stderr(predicate::str::contains("no key given"));
        harness(h)
            .args(["auth", "set-key", "--provider", "nope", "--stdin"])
            .write_stdin("k\n")
            .assert()
            .code(2)
            .stderr(predicate::str::contains("unknown provider"));
        // Nothing in the CLI or daemon logs either.
        for entry in std::fs::read_dir(data.join("logs")).unwrap() {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(!text.contains("sk-ant-test"), "{}", path.display());
        }

        // A wrong token is exit 4.
        let mut forged = info.clone();
        forged.token = "wrong".into();
        forged.write(&data).unwrap();
        harness(h)
            .args(["session", "list"])
            .assert()
            .code(4)
            .stderr(predicate::str::contains("[unauthorized]"));
        info.write(&data).unwrap();

        // `--no-spawn` is fine while it runs; `daemon stop` ends it.
        harness(h)
            .args(["--no-spawn", "session", "list"])
            .assert()
            .success();
        harness(h)
            .args(["daemon", "stop"])
            .assert()
            .success()
            .stdout(predicate::str::contains(format!(
                "daemon stopped (pid {})",
                info.pid
            )));
        assert!(DaemonInfo::read(&data).unwrap().is_none());
        harness(h)
            .args(["--no-spawn", "session", "list"])
            .assert()
            .code(3);
    });
    stop(h);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

#[test]
fn daemon_run_serves_in_the_foreground_until_stopped() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let data = h.join("data");
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("harness"))
        .arg("--home")
        .arg(h)
        .args(["daemon", "run"])
        .env_remove("HARNESS_LOG_LEVEL")
        .env("HARNESS_DAEMON_PATH", harnessd())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while DaemonInfo::read(&data).unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let result = std::panic::catch_unwind(|| {
        let info = DaemonInfo::read(&data).unwrap().expect("daemon.json");
        assert_ne!(
            info.pid,
            child.id(),
            "the CLI must run harnessd, not become it"
        );
        harness(h)
            .args(["--no-spawn", "daemon", "status"])
            .assert()
            .success();
        harness(h).args(["daemon", "stop"]).assert().success();
    });
    // `harness daemon run` exits with the daemon.
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("harness daemon run did not exit after daemon stop");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
    assert!(status.success(), "{status}");
}
