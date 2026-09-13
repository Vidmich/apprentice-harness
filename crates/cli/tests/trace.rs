//! `harness trace export | import | replay-check` against a router on a
//! real local socket (task M01-14). The daemon side is faked: params are
//! echoed into the result so the test can check what the CLI sent —
//! the absolute output path above all — and the exit code of a failed
//! replay-check.

use std::path::Path;
use std::sync::Arc;

use apprentice_api::methods::{
    TraceExport, TraceExportResult, TraceImport, TraceImportResult, TraceReplayCheck,
};
use apprentice_api::server::{Router, RouterConfig};
use apprentice_api::transport::Endpoint;
use apprentice_api::types::{
    BundleCounts, BundleManifest, BundleSelection, ImportedSession, RedactionReport, RedactionRule,
    ReplayCall, ReplayReport, ReplayStatus,
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

fn counts() -> BundleCounts {
    BundleCounts {
        sessions: 1,
        workspaces: 1,
        agents: 1,
        steps: 3,
        events: 25,
        mentor_calls: 3,
        messages: 6,
        blobs: 16,
        blob_bytes: 53_038,
    }
}

#[test]
fn without_daemon_the_command_fails_with_a_hint() {
    let home = tempfile::tempdir().unwrap();
    harness(home.path())
        .args(["--no-spawn", "trace", "replay-check"])
        .assert()
        .code(3)
        .stderr(predicates::str::contains("no daemon is running"));
}

#[tokio::test(flavor = "multi_thread")]
async fn bundle_commands_over_a_live_router() {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();

    let mut router = Router::new(RouterConfig {
        daemon_version: "9.9.9".into(),
        pid: std::process::id(),
        token: Some("secret".into()),
    });
    router.add::<TraceExport, _, _>(|_c, p| async move {
        Ok(TraceExportResult {
            path: p.output.clone(),
            manifest: BundleManifest {
                format_version: 1,
                created_at: "2026-09-13T10:00:00.000Z".into(),
                harness_version: "0.1.0".into(),
                schema_version: 3,
                selection: BundleSelection {
                    session_ids: p.session_ids.clone(),
                    workspace_id: p.workspace_id.clone(),
                    since: p.since.clone(),
                    until: p.until.clone(),
                    all: p.all,
                },
                sessions: vec![],
                counts: counts(),
                redaction: p.redact.then(|| RedactionReport {
                    applied: true,
                    rules: vec![
                        RedactionRule {
                            name: "anthropic_key".into(),
                            source: "builtin".into(),
                            matches: 2,
                        },
                        RedactionRule {
                            name: "paths".into(),
                            source: "paths".into(),
                            matches: u64::from(p.redact_paths) * 8,
                        },
                    ],
                    replacements: 2 + u64::from(p.redact_paths) * 8,
                    secrets: 1,
                    paths: p.redact_paths,
                    blob_map: std::collections::BTreeMap::new(),
                    touched_requests: 3,
                    replayable: false,
                }),
            },
        })
    });
    router.add::<TraceImport, _, _>(|_c, p| async move {
        Ok(TraceImportResult {
            sessions: vec![ImportedSession {
                from: "s1".into(),
                to: if p.keep_ids { "s1".into() } else { "s9".into() },
            }],
            counts: counts(),
            redacted: p.into_workspace.is_some(),
            blobs_written: 16,
        })
    });
    router.add::<TraceReplayCheck, _, _>(|_c, p| async move {
        let failed = p.rebuild;
        Ok(ReplayReport {
            calls: vec![ReplayCall {
                call_id: "m2".into(),
                session_id: p.session_id.clone().unwrap_or_else(|| "s1".into()),
                agent_id: "a1".into(),
                step_id: "st2".into(),
                kind: "step".into(),
                request_event_id: "e12".into(),
                status: if failed {
                    ReplayStatus::Failed
                } else {
                    ReplayStatus::Ok
                },
                checks: vec!["blob".into(), "hash".into()],
                problems: if failed {
                    vec!["body hashes to bbbb, not its id aaaa".into()]
                } else {
                    vec![]
                },
            }],
            checked: 1,
            passed: u64::from(!failed),
            failed: u64::from(failed),
            skipped: 0,
            rebuild: p.rebuild,
        })
    });
    let router = Arc::new(router);
    let endpoint = Endpoint::default_for(&data, &format!("trace-live-{}", std::process::id()));
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
    let cwd = tempfile::tempdir().unwrap();
    let cwd_path = cwd.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        // The output path goes out absolute, resolved from our cwd; the
        // default name is dated.
        let out = harness(&home_path)
            .current_dir(&cwd_path)
            .args([
                "trace",
                "export",
                "--session",
                "s1",
                "--since",
                "7d",
                "--redact-paths",
                "-o",
                "week.tar.zst",
                "--json",
            ])
            .assert()
            .success();
        let r: TraceExportResult = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert_eq!(
            Path::new(&r.path),
            std::path::absolute(cwd_path.join("week.tar.zst")).unwrap()
        );
        assert_eq!(r.manifest.selection.session_ids, ["s1"]);
        assert_eq!(r.manifest.selection.since.as_deref(), Some("7d"));
        assert!(r.manifest.redaction.as_ref().unwrap().paths);
        let out = harness(&home_path)
            .current_dir(&cwd_path)
            .args(["trace", "export", "--all", "--no-redact", "--json"])
            .assert()
            .success();
        let r: TraceExportResult = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert!(r.manifest.redaction.is_none());
        assert!(r.manifest.selection.all);
        let name = Path::new(&r.path).file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("harness-traces-") && name.ends_with("Z.tar.zst"),
            "{name}"
        );

        // Human output: where, the counts, the rules that matched.
        harness(&home_path)
            .current_dir(&cwd_path)
            .args(["trace", "export", "--workspace", "w1", "-o", "bundle"])
            .assert()
            .success()
            .stdout(predicates::str::contains("wrote "))
            .stdout(predicates::str::contains(
                "sessions 1 · workspaces 1 · agents 1 · steps 3 · events 25 · mentor calls 3 · messages 6 · blobs 16 (51.8 KiB)\n",
            ))
            .stdout(predicates::str::contains(
                "redaction: 2 replacements (1 distinct secrets); 3 request bodies touched — not byte-replayable\n  anthropic_key  builtin    2\n",
            ))
            .stdout(predicates::str::contains("paths").not());

        harness(&home_path)
            .current_dir(&cwd_path)
            .args(["trace", "import", "week.tar.zst"])
            .assert()
            .success()
            .stdout(
                "imported 1 session (new ids: s1 → s9)\n\
                 agents 1 · steps 3 · events 25 · mentor calls 3 · messages 6 · blobs 16 (16 new)\n",
            );
        harness(&home_path)
            .current_dir(&cwd_path)
            .args([
                "trace",
                "import",
                "week.tar.zst",
                "--keep-ids",
                "--into-workspace",
                "w1",
            ])
            .assert()
            .success()
            .stdout(predicates::str::starts_with("imported 1 session\n"))
            .stdout(predicates::str::contains("the bundle was redacted"));
        let out = harness(&home_path)
            .current_dir(&cwd_path)
            .args(["trace", "import", "week.tar.zst", "--json"])
            .assert()
            .success();
        let r: TraceImportResult = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert_eq!(r.sessions[0].to, "s9");

        // replay-check: exit 0 when everything passes, 2 with a failure
        // (the report is printed either way, JSON included).
        harness(&home_path)
            .args(["trace", "replay-check", "--session", "s1"])
            .assert()
            .success()
            .stdout("checked 1 · passed 1 · failed 0 · skipped 0\n");
        harness(&home_path)
            .args(["trace", "replay-check", "--rebuild"])
            .assert()
            .code(2)
            .stdout(
                "FAILED  m2 (step · session s1 · hash)\n        body hashes to bbbb, not its id aaaa\n\
                 checked 1 · passed 0 · failed 1 · skipped 0 · rebuilt\n",
            )
            .stderr(predicates::str::contains("1 of 1 calls failed replay-check"));
        let out = harness(&home_path)
            .args(["trace", "replay-check", "--rebuild", "--json"])
            .assert()
            .code(2);
        let r: ReplayReport = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert_eq!(r.failed, 1);
        assert!(r.rebuild);
    })
    .await
    .unwrap();
    serve.abort();
}
