//! `harnessd` initialises logging from flags, environment and config
//! (task M00-04). Lifecycle itself is M00-08.

use std::path::Path;

use assert_cmd::Command;

fn daemon_log(home: &Path) -> String {
    let logs = home.join("data").join("logs");
    let mut files: Vec<_> = std::fs::read_dir(&logs)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("daemon.")
        })
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    std::fs::read_to_string(files.remove(0)).unwrap()
}

fn json_lines(text: &str) -> Vec<serde_json::Value> {
    text.lines()
        .map(|l| serde_json::from_str(l).expect("json line"))
        .collect()
}

#[test]
fn writes_json_log_with_flag_over_env_over_config() {
    let home = tempfile::tempdir().unwrap();

    // config only -> warn: the info "starting" line is filtered out.
    std::fs::write(
        home.path().join("config.toml"),
        "[daemon]\nlog_level = \"warn\"\n",
    )
    .unwrap();
    Command::cargo_bin("harnessd")
        .unwrap()
        .arg("--home")
        .arg(home.path())
        .env_remove("HARNESS_LOG_LEVEL")
        .env_remove("RUST_LOG")
        .assert()
        .success();
    let text = daemon_log(home.path());
    let lines = json_lines(&text);
    assert!(lines.iter().all(|l| l["level"] == "WARN"), "{text}");
    assert!(text.contains("lifecycle not implemented"), "{text}");

    // env beats config.
    Command::cargo_bin("harnessd")
        .unwrap()
        .arg("--home")
        .arg(home.path())
        .env("HARNESS_LOG_LEVEL", "info")
        .env_remove("RUST_LOG")
        .assert()
        .success();
    let text = daemon_log(home.path());
    let starting = json_lines(&text)
        .into_iter()
        .find(|l| l["fields"]["message"] == "starting")
        .expect("starting line at info");
    assert_eq!(starting["fields"]["filter"], "info");
    assert_eq!(starting["fields"]["api_version"], 1);
    assert!(
        starting["fields"]["log_file"]
            .as_str()
            .unwrap()
            .contains("daemon.")
    );

    // flag beats env.
    Command::cargo_bin("harnessd")
        .unwrap()
        .args([
            "--home",
            home.path().to_str().unwrap(),
            "--log-level",
            "trace",
        ])
        .env("HARNESS_LOG_LEVEL", "warn")
        .env_remove("RUST_LOG")
        .assert()
        .success();
    let text = daemon_log(home.path());
    assert!(
        json_lines(&text)
            .iter()
            .any(|l| l["fields"]["filter"] == "trace"),
        "{text}"
    );
    // The debug line from telemetry::init itself is present at trace.
    assert!(text.contains("logging initialised"), "{text}");
}

#[test]
fn broken_config_still_logs_and_reports() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[mentor]\nmodle = 1\n").unwrap();
    Command::cargo_bin("harnessd")
        .unwrap()
        .arg("--home")
        .arg(home.path())
        .arg("--foreground")
        .env_remove("HARNESS_LOG_LEVEL")
        .assert()
        .success()
        .stderr(predicates::str::contains("configuration is invalid"));
    let text = daemon_log(home.path());
    assert!(text.contains("mentor.modle"), "{text}");
}
