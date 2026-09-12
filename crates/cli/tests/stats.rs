//! `harness stats tokens|reprice` against a router on a real local socket
//! (task M00-07). The daemon side is faked: params are echoed into the
//! result so the test can check they were passed through verbatim.

use std::path::Path;
use std::sync::Arc;

use apprentice_api::methods::{StatsReprice, StatsRepriceResult, StatsTokens};
use apprentice_api::server::{Router, RouterConfig};
use apprentice_api::transport::Endpoint;
use apprentice_api::types::{ApprenticeStats, StatsRange, TokenBucket, TokenStats};
use apprentice_client::DaemonInfo;
use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;

fn harness(home: &Path) -> Command {
    let mut c = Command::cargo_bin("harness").unwrap();
    c.arg("--home")
        .arg(home)
        .arg("--quiet")
        .env_remove("HARNESS_LOG_LEVEL");
    c
}

fn bucket(key: Option<&str>, label: Option<&str>) -> TokenBucket {
    TokenBucket {
        key: key.map(str::to_owned),
        label: label.map(str::to_owned),
        calls: 14,
        input: 182_340,
        output: 21_004,
        cache_read: 610_222,
        cache_creation: 0,
        cost_usd: 1.84,
        unpriced_calls: 0,
    }
}

#[test]
fn without_daemon_the_command_fails_with_a_hint() {
    let home = tempfile::tempdir().unwrap();
    harness(home.path())
        .args(["--no-spawn", "stats", "tokens"])
        .assert()
        .code(3)
        .stderr(predicates::str::contains("no daemon is running"));
}

#[tokio::test(flavor = "multi_thread")]
async fn tokens_and_reprice_over_a_live_router() {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();

    let mut router = Router::new(RouterConfig {
        daemon_version: "9.9.9".into(),
        pid: std::process::id(),
        token: Some("secret".into()),
    });
    router.add::<StatsTokens, _, _>(|_c, p| async move {
        Ok(TokenStats {
            range: StatsRange {
                since: p.since.clone(),
                until: p.until.clone(),
            },
            tz: "+02:00".into(),
            totals: bucket(None, None),
            by_model: vec![bucket(Some("claude-opus-5"), None)],
            by_day: vec![bucket(Some("2026-09-11"), None)],
            by_session: if p.session_id.is_some() {
                vec![]
            } else {
                vec![bucket(Some("s1"), Some("alpha"))]
            },
            apprentice: ApprenticeStats::default(),
        })
    });
    router.add::<StatsReprice, _, _>(|_c, p| async move {
        Ok(StatsRepriceResult {
            examined: 4,
            changed: u64::from(p.model.is_some()),
            unpriced: 1,
        })
    });
    let router = Arc::new(router);
    let endpoint = Endpoint::default_for(&data, &format!("stats-live-{}", std::process::id()));
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
    tokio::task::spawn_blocking(move || {
        // --json output is the wire type verbatim.
        let out = harness(&home_path)
            .args(["stats", "tokens", "--since", "7d", "--json"])
            .assert()
            .success();
        let stats: TokenStats = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert_eq!(stats.range.since.as_deref(), Some("7d"));
        assert_eq!(stats.totals.calls, 14);
        assert_eq!(stats.by_session[0].label.as_deref(), Some("alpha"));

        // Human output: table plus the one-line total.
        harness(&home_path)
            .args([
                "stats",
                "tokens",
                "--session",
                "s1",
                "--by",
                "day",
                "--by",
                "session",
            ])
            .assert()
            .success()
            .stdout(predicates::str::contains(
                "range beginning → now (days in +02:00)",
            ))
            .stdout(predicates::str::contains("2026-09-11     14  182,340"))
            .stdout(predicates::str::contains(
                "14 calls · in 182,340 · out 21,004 · cache read 610,222 · $1.84\n",
            ))
            .stdout(predicates::str::contains("session").not());

        harness(&home_path)
            .args(["stats", "reprice", "--model", "claude-opus-5"])
            .assert()
            .success()
            .stdout("repriced 1 of 4 calls · 1 unpriced (no [pricing] entry)\n");
        let out = harness(&home_path)
            .args(["stats", "reprice", "--json"])
            .assert()
            .success();
        let r: StatsRepriceResult = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert_eq!(r.changed, 0);
    })
    .await
    .unwrap();
    serve.abort();
}
