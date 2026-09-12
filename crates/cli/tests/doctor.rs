//! `harness doctor` with no daemon, a stale `daemon.json`, and a live
//! router on a real local socket (task M00-04).

use std::path::Path;
use std::sync::Arc;

use apprentice_api::methods::{
    AuthStatus, AuthStatusResult, DaemonStatus, DaemonStatusResult, Empty, ProviderAuth,
};
use apprentice_api::server::{Router, RouterConfig};
use apprentice_api::transport::Endpoint;
use apprentice_client::DaemonInfo;
use assert_cmd::Command;
use serde_json::Value;

fn doctor(home: &Path) -> Command {
    let mut c = Command::cargo_bin("harness").unwrap();
    c.arg("--home")
        .arg(home)
        .arg("--quiet")
        .env_remove("HARNESS_LOG_LEVEL")
        .env_remove("ANTHROPIC_API_KEY");
    c
}

fn doctor_json(home: &Path) -> Value {
    let out = doctor(home).args(["doctor", "--json"]).assert().success();
    serde_json::from_slice(&out.get_output().stdout).unwrap()
}

#[test]
fn without_daemon_text_and_json() {
    let home = tempfile::tempdir().unwrap();
    doctor(home.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "Daemon\n  state          not_running",
        ))
        .stdout(predicates::str::contains("Auth (via unknown)"));

    let v = doctor_json(home.path());
    assert_eq!(v["daemon"]["state"], "not_running");
    assert_eq!(v["auth"]["providers"][0]["configured"], false);
    assert_eq!(v["harness"]["api_version"], 1);
    assert!(
        v["paths"]["cli_log_file"]
            .as_str()
            .unwrap()
            .contains("cli.")
    );
    assert!(v["paths"]["disk_free_bytes"].as_u64().is_some());

    // The CLI wrote its own JSON log under the home.
    let logs = home.path().join("data").join("logs");
    let cli_log = std::fs::read_dir(&logs)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.file_name().unwrap().to_string_lossy().starts_with("cli."))
        .expect("cli log file");
    let text = std::fs::read_to_string(cli_log).unwrap();
    assert!(
        text.is_empty() || text.lines().all(|l| l.starts_with('{')),
        "{text}"
    );

    // ANTHROPIC_API_KEY in the environment is visible without a daemon.
    let out = doctor(home.path())
        .args(["doctor", "--json"])
        .env("ANTHROPIC_API_KEY", "sk-ant-env")
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["auth"]["checked_via"], "env");
    assert_eq!(v["auth"]["providers"][0]["source"], "env");
    assert!(!String::from_utf8_lossy(&out.get_output().stdout).contains("sk-ant-env"));
}

#[test]
fn stale_daemon_json_is_reported() {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let info = DaemonInfo {
        pid: u32::MAX - 1,
        endpoint: Endpoint::default_for(&data, "doctor-stale-test"),
        token: "t".into(),
        api_version: 1,
        version: "0.0.1".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
    };
    std::fs::write(
        DaemonInfo::path(&data),
        serde_json::to_string(&info).unwrap(),
    )
    .unwrap();

    let v = doctor_json(home.path());
    assert_eq!(v["daemon"]["state"], "stale");
    assert_eq!(v["daemon"]["pid_alive"], false);
    assert!(v["daemon"]["error"].as_str().unwrap().len() > 3);
    assert!(
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("did not answer"))
    );

    std::fs::write(DaemonInfo::path(&data), "{ not json").unwrap();
    let v = doctor_json(home.path());
    assert_eq!(v["daemon"]["state"], "unreadable");
}

#[tokio::test(flavor = "multi_thread")]
async fn running_daemon_is_queried() {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();

    let mut router = Router::new(RouterConfig {
        daemon_version: "9.9.9".into(),
        pid: std::process::id(),
        token: Some("secret".into()),
    });
    router.add::<DaemonStatus, _, _>(|_c, Empty {}| async {
        Ok(DaemonStatusResult {
            version: "9.9.9".into(),
            pid: std::process::id(),
            uptime_s: 12,
            sessions_open: 0,
            data_dir: "/d".into(),
            log_file: Some("/d/logs/daemon.log".into()),
        })
    });
    router.add::<AuthStatus, _, _>(|_c, Empty {}| async {
        Ok(AuthStatusResult {
            providers: vec![ProviderAuth {
                name: "anthropic".into(),
                configured: true,
                source: Some("keychain".into()),
            }],
        })
    });
    let router = Arc::new(router);
    let endpoint = Endpoint::default_for(&data, &format!("doctor-live-{}", std::process::id()));
    let listener = endpoint.listen().unwrap();
    let serve = tokio::spawn({
        let router = Arc::clone(&router);
        async move {
            loop {
                let (r, w) = listener.accept().await.unwrap();
                let router = Arc::clone(&router);
                tokio::spawn(async move {
                    let _ = router.serve(r, w).await;
                });
            }
        }
    });

    let info = DaemonInfo {
        pid: std::process::id(),
        endpoint: endpoint.clone(),
        token: "secret".into(),
        api_version: 1,
        version: "9.9.9".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
    };
    std::fs::write(
        DaemonInfo::path(&data),
        serde_json::to_string(&info).unwrap(),
    )
    .unwrap();

    let home_path = home.path().to_path_buf();
    let v = tokio::task::spawn_blocking(move || doctor_json(&home_path))
        .await
        .unwrap();
    assert_eq!(v["daemon"]["state"], "running", "{v}");
    assert_eq!(v["daemon"]["pid_alive"], true);
    assert_eq!(v["daemon"]["version"], "9.9.9");
    assert_eq!(v["daemon"]["uptime_s"], 12);
    assert_eq!(v["daemon"]["log_file"], "/d/logs/daemon.log");
    assert_eq!(v["auth"]["checked_via"], "daemon");
    assert_eq!(v["auth"]["providers"][0]["source"], "keychain");
    serve.abort();
}
