//! `harness stats tokens|calls|reprice` against a router on a real local
//! socket (tasks M00-07, M01-13). The daemon side is faked: params are
//! echoed into the result so the test can check they were passed through
//! verbatim.

use std::path::Path;
use std::sync::Arc;

use apprentice_api::methods::{
    StatsCalls, StatsCallsResult, StatsReprice, StatsRepriceResult, StatsTokens,
};
use apprentice_api::server::{Router, RouterConfig};
use apprentice_api::transport::Endpoint;
use apprentice_api::types::{
    ApprenticeStats, CallSummary, StatsGroup, StatsRange, TokenBucket, TokenStats,
};
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
            by_workspace: if p.group_by.contains(&StatsGroup::Workspace) {
                vec![bucket(Some("w1"), Some("C:/src/alpha"))]
            } else {
                vec![]
            },
            by_kind: if p.group_by.contains(&StatsGroup::Kind) {
                vec![bucket(Some("title"), None)]
            } else {
                vec![]
            },
            apprentice: ApprenticeStats::default(),
        })
    });
    router.add::<StatsCalls, _, _>(|_c, p| async move {
        Ok(StatsCallsResult {
            calls: vec![CallSummary {
                call_id: "m1".into(),
                started_at: "2026-09-12T10:04:00.000Z".into(),
                session_id: p.session_id.clone().unwrap_or_else(|| "s1".into()),
                session_title: Some("alpha, the first".into()),
                workspace_id: p.workspace_id.clone(),
                agent_id: "a1".into(),
                kind: "step".into(),
                model: "claude-opus-5".into(),
                effort: Some("high".into()),
                status: "ok".into(),
                stop_reason: Some("end_turn".into()),
                input: 1204,
                output: 310,
                cache_read: 0,
                cache_creation: 900,
                cost_usd: Some(0.0138),
                total_ms: Some(4200),
                request_event_id: "e7".into(),
            }],
            total: 7 + p.offset,
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

        // The new breakdowns are asked for by `--by`.
        harness(&home_path)
            .args(["stats", "tokens", "--by", "workspace", "--by", "kind"])
            .assert()
            .success()
            .stdout(predicates::str::contains("w1 (C:/src/alpha)"))
            .stdout(predicates::str::contains("\ntitle "))
            .stdout(predicates::str::contains("model").not());

        // `stats calls`: table, CSV and JSON.
        harness(&home_path)
            .args(["stats", "calls", "--session", "s9", "--offset", "5"])
            .assert()
            .success()
            .stdout(predicates::str::contains(
                "2026-09-12T10:04:00.000Z  alpha, the first  step  claude-opus-5  ok      1,204  310         0  $0.0138  4,200\n",
            ))
            .stdout(predicates::str::ends_with("calls 6–6 of 12\n"));
        harness(&home_path)
            .args(["stats", "calls", "--workspace", "w1", "--csv"])
            .assert()
            .success()
            .stdout(
                "call_id,started_at,session_id,session_title,workspace_id,agent_id,kind,model,effort,status,stop_reason,input,output,cache_read,cache_creation,cost_usd,total_ms,request_event_id\r\n\
                 m1,2026-09-12T10:04:00.000Z,s1,\"alpha, the first\",w1,a1,step,claude-opus-5,high,ok,end_turn,1204,310,0,900,0.0138,4200,e7\r\n",
            );
        let out = harness(&home_path)
            .args(["stats", "calls", "--json"])
            .assert()
            .success();
        let r: StatsCallsResult = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert_eq!(r.total, 7);
        assert_eq!(r.calls[0].call_id, "m1");

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
