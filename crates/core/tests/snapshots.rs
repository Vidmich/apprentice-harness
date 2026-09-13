//! Workspace snapshots (task M01-06) through the trace store: the
//! `workspace.snapshot` pair an agent run records around its tool
//! calls, the diff blob, and the `files_changed` outcome derived from
//! the pair. The runtime of M01-08 does exactly this sequence.

use std::path::Path;
use std::sync::Arc;

use apprentice_api::types::TraceEvent;
use apprentice_core::config::{Paths, ToolsConfig};
use apprentice_core::tools::{
    AllowAll, Executor, SeenFiles, ToolCall, ToolRegistry, ToolResultKind, builtin_tools,
};
use apprentice_core::trace::{
    BlobId, NewAgent, NewSession, StepRef, TraceStore, TraceWriter, kinds,
};
use apprentice_core::workspace::{SnapshotPhase, Workspace};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn git(dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_DATE", "2024-01-02T10:00:00+0000")
        .env("GIT_COMMITTER_DATE", "2024-01-02T10:00:00+0000")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[tokio::test]
async fn an_agent_run_records_two_snapshots_and_a_files_changed_outcome() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/x.rs"), "fn one() -> i32 {\n    1\n}\n").unwrap();
    std::fs::write(root.join("README.md"), "# x\n").unwrap();
    if !git(root, &["init", "-q"]) {
        eprintln!("skipping: git is not on PATH");
        return;
    }
    for args in [
        ["config", "user.name", "Test User"],
        ["config", "user.email", "test@example.com"],
        ["config", "commit.gpgsign", "false"],
        ["add", ".", ""],
        ["commit", "-qm", "init"],
    ] {
        let args: Vec<&str> = args.iter().copied().filter(|a| !a.is_empty()).collect();
        assert!(git(root, &args), "git {args:?}");
    }

    let home = tempfile::tempdir().unwrap();
    let paths = Paths::from_home(home.path());
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    let store = Arc::new(TraceStore::open(&paths).unwrap());
    let workspace = Arc::new(Workspace::open(root).unwrap());
    let session = store
        .create_session(&NewSession {
            title: Some("snapshots".into()),
            workspace_path: Some(workspace.root_string()),
            workspace_id: None,
            config: json!({}),
        })
        .unwrap();
    let agent = store
        .start_agent(&NewAgent::main(session.clone(), "task"))
        .unwrap();
    let step = store.start_step(&agent).unwrap();
    let writer = TraceWriter::spawn(Arc::clone(&store));
    let registry = ToolRegistry::new();
    registry.register_all(builtin_tools()).unwrap();
    let config = ToolsConfig::default();
    let at = StepRef {
        session: session.clone(),
        agent: agent.clone(),
        step,
    };

    // agent.started → snapshot(start)
    let start = workspace.snapshot().await;
    store
        .append(start.event(session.clone(), Some(agent.clone()), SnapshotPhase::Start))
        .unwrap();

    // The agent edits a file and creates another through the tools.
    let executor = Executor::new(
        &registry,
        &AllowAll,
        &writer,
        &config,
        at.clone(),
        CancellationToken::new(),
    )
    .with_workspace(Some(Arc::clone(&workspace)))
    .with_seen_files(Arc::new(SeenFiles::new()));
    let edited = executor
        .execute_one(ToolCall::new(
            "c1",
            "edit_file",
            json!({"path": "src/x.rs", "old_string": "    1\n", "new_string": "    2\n"}),
        ))
        .await;
    assert_eq!(edited.kind, ToolResultKind::Ok, "{edited:?}");
    let written = executor
        .execute_one(ToolCall::new(
            "c2",
            "write_file",
            json!({"path": "src/y.rs", "content": "fn two() {}\n"}),
        ))
        .await;
    assert_eq!(written.kind, ToolResultKind::Ok, "{written:?}");
    writer.flush().await.unwrap();

    // snapshot(end) → outcome → agent.finished
    let end = workspace.snapshot().await;
    store
        .append(end.event(session.clone(), Some(agent.clone()), SnapshotPhase::End))
        .unwrap();
    let changes = end.changes_since(&start);
    store
        .append(changes.event(session.clone(), Some(agent.clone())))
        .unwrap();

    let events: Vec<TraceEvent> = store.session_events(&session).unwrap();
    let of = |kind: &str| -> Vec<&TraceEvent> {
        events.iter().filter(|e| e.summary.kind == kind).collect()
    };
    let snaps = of(kinds::WORKSPACE_SNAPSHOT);
    assert_eq!(snaps.len(), 2);
    let (first, last) = (snaps[0], snaps[1]);
    assert_eq!(first.payload["phase"], "start");
    assert_eq!(first.payload["dirty"], false);
    assert_eq!(first.payload["file_count"], 2);
    assert_eq!(first.payload["git_head"].as_str().unwrap().len(), 40);
    assert!(first.blob_id.is_none(), "a clean tree has no diff blob");
    assert_eq!(last.payload["phase"], "end");
    assert_eq!(last.payload["dirty"], true);
    assert_eq!(last.payload["file_count"], 3);
    assert_eq!(last.payload["git_head"], first.payload["git_head"]);
    assert_ne!(last.payload["index_hash"], first.payload["index_hash"]);
    assert_ne!(last.payload["status_hash"], first.payload["status_hash"]);
    assert_eq!(
        last.payload["untracked"],
        json!([{"path": "src/y.rs", "bytes": 12}])
    );
    let diff_id = BlobId::from(last.blob_id.as_deref().expect("the end diff is a blob"));
    let diff = String::from_utf8(store.read_blob(&diff_id).unwrap()).unwrap();
    assert!(diff.contains("+++ b/src/x.rs"), "{diff}");
    assert!(diff.contains("-    1\n+    2\n"), "{diff}");
    assert_eq!(last.payload["diff_bytes"], diff.len());
    assert_eq!(
        store.blob_meta(&diff_id).unwrap().media_type,
        apprentice_core::workspace::DIFF_MEDIA_TYPE
    );

    let outcomes = of(kinds::OUTCOME);
    assert_eq!(outcomes.len(), 1);
    let outcome = &outcomes[0].payload;
    assert_eq!(outcome["kind"], "files_changed");
    assert_eq!(outcome["details"]["changed"], json!(["src/x.rs"]));
    assert_eq!(outcome["details"]["added"], json!(["src/y.rs"]));
    assert_eq!(outcome["details"]["deleted"], json!([]));
    assert_eq!(
        outcomes[0].summary.agent_id.as_deref(),
        Some(agent.as_str())
    );

    // A second, idle agent: the pair agrees and the outcome is empty.
    let again = workspace.snapshot().await;
    assert!(again.changes_since(&end).is_empty());
    assert_eq!(again.index_hash, end.index_hash);
    let ev = again.changes_since(&end).event(session.clone(), None);
    assert_eq!(
        ev.payload["details"]["counts"],
        json!({"changed": 0, "added": 0, "deleted": 0})
    );

    writer.shutdown().await;
}
