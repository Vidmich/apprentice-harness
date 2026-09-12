//! The file tools (task M01-03) through the execution wrapper: what the
//! trace keeps (`tool.call` blob = exact input, `tool.result` blob =
//! the diff), the sandbox as the mentor meets it, and the seen-files
//! set across steps.

use std::path::Path;
use std::sync::Arc;

use apprentice_api::types::TraceEvent;
use apprentice_core::config::{Paths, ToolsConfig};
use apprentice_core::mentor::{ContentBlock, ToolResultContent};
use apprentice_core::tools::{
    AllowAll, Executed, Executor, SeenFiles, ToolCall, ToolRegistry, ToolResultKind, file_tools,
};
use apprentice_core::trace::{
    BlobId, NewAgent, NewSession, StepRef, TraceStore, TraceWriter, kinds,
};
use apprentice_core::workspace::Workspace;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct Harness {
    _home: tempfile::TempDir,
    repo: tempfile::TempDir,
    store: Arc<TraceStore>,
    writer: TraceWriter,
    registry: ToolRegistry,
    config: ToolsConfig,
    at: StepRef,
    workspace: Arc<Workspace>,
    seen: Arc<SeenFiles>,
}

impl Harness {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        std::fs::write(
            repo.path().join("src/x.rs"),
            "fn one() -> i32 {\n    1\n}\n\nfn two() -> i32 {\n    2\n}\n",
        )
        .unwrap();
        let workspace = Arc::new(Workspace::open(repo.path()).unwrap());
        let paths = Paths::from_home(home.path());
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        let store = Arc::new(TraceStore::open(&paths).unwrap());
        let session = store
            .create_session(&NewSession {
                title: Some("file tools".into()),
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
        registry.register_all(file_tools()).unwrap();
        Self {
            _home: home,
            repo,
            store,
            writer,
            registry,
            config: ToolsConfig::default(),
            at: StepRef {
                session,
                agent,
                step,
            },
            workspace,
            seen: Arc::new(SeenFiles::new()),
        }
    }

    fn executor(&self) -> Executor<'_> {
        Executor::new(
            &self.registry,
            &AllowAll,
            &self.writer,
            &self.config,
            self.at.clone(),
            CancellationToken::new(),
        )
        .with_workspace(Some(Arc::clone(&self.workspace)))
        .with_seen_files(Arc::clone(&self.seen))
    }

    async fn run(&self, id: &str, name: &str, input: Value) -> Executed {
        self.executor()
            .execute_one(ToolCall::new(id, name, input))
            .await
    }

    async fn pair(&self, call_id: &str) -> (TraceEvent, TraceEvent) {
        self.writer.flush().await.unwrap();
        let events: Vec<TraceEvent> = self
            .store
            .session_events(&self.at.session)
            .unwrap()
            .into_iter()
            .filter(|e| e.summary.kind.starts_with("tool."))
            .collect();
        let find = |kind: &str| {
            events
                .iter()
                .find(|e| e.summary.kind == kind && e.payload["call_id"] == call_id)
                .cloned()
                .unwrap_or_else(|| panic!("no {kind} for {call_id}"))
        };
        (find(kinds::TOOL_CALL), find(kinds::TOOL_RESULT))
    }

    fn blob(&self, event: &TraceEvent) -> String {
        let id = BlobId::from(event.blob_id.as_deref().expect("event has a blob"));
        String::from_utf8(self.store.read_blob(&id).unwrap()).unwrap()
    }

    fn file(&self, rel: &str) -> String {
        std::fs::read_to_string(self.repo.path().join(rel)).unwrap()
    }

    async fn close(self) {
        self.writer.shutdown().await;
    }
}

fn text_of(block: &ContentBlock) -> (String, bool) {
    match block {
        ContentBlock::ToolResult {
            content, is_error, ..
        } => {
            let text = content
                .iter()
                .map(|c| match c {
                    ToolResultContent::Text { text } => text.clone(),
                    other => panic!("unexpected content {other:?}"),
                })
                .collect();
            (text, *is_error)
        }
        other => panic!("not a tool_result: {other:?}"),
    }
}

#[tokio::test]
async fn edit_leaves_the_exact_input_and_the_diff_in_the_trace() {
    let h = Harness::new();
    let read = h.run("c1", "read_file", json!({"path": "src/x.rs"})).await;
    assert_eq!(read.kind, ToolResultKind::Ok);
    assert_eq!(read.summary, "read src/x.rs lines 1–7 of 7");
    let (text, is_error) = text_of(&read.block);
    assert!(!is_error);
    assert_eq!(
        text,
        "1\tfn one() -> i32 {\n2\t    1\n3\t}\n4\t\n5\tfn two() -> i32 {\n6\t    2\n7\t}\n"
    );

    let input = json!({
        "path": "src/x.rs",
        "old_string": "    2\n",
        "new_string": "    1 + 1\n"
    });
    let edit = h.run("c2", "edit_file", input.clone()).await;
    assert_eq!(edit.kind, ToolResultKind::Ok, "{:?}", text_of(&edit.block));
    assert_eq!(edit.summary, "edited src/x.rs (+1 −1)");
    assert_eq!(
        h.file("src/x.rs"),
        "fn one() -> i32 {\n    1\n}\n\nfn two() -> i32 {\n    1 + 1\n}\n"
    );

    let (call, result) = h.pair("c2").await;
    assert_eq!(h.blob(&call), serde_json::to_string(&input).unwrap());
    assert_eq!(call.payload["risk"], "write");
    let diff = h.blob(&result);
    assert!(
        diff.starts_with(
            "edited src/x.rs (+1 −1, 1 replacement)\n\n--- a/src/x.rs\n+++ b/src/x.rs\n"
        ),
        "{diff}"
    );
    assert!(diff.contains("\n-    2\n+    1 + 1\n"), "{diff}");
    assert!(!diff.contains("note:"), "{diff}");
    assert_eq!(result.payload["metadata"]["replacements"], 1);
    assert_eq!(result.payload["metadata"]["added"], 1);
    assert_eq!(result.payload["summary"], "edited src/x.rs (+1 −1)");
    let (mentor, is_error) = text_of(&edit.block);
    assert!(!is_error);
    assert_eq!(mentor, diff);

    // An error result is still a result: the diagnostic is the blob.
    let bad = h
        .run(
            "c3",
            "edit_file",
            json!({"path": "src/x.rs", "old_string": "fn two() -> i64 {", "new_string": "x"}),
        )
        .await;
    assert_eq!(bad.kind, ToolResultKind::Error);
    let (_, result) = h.pair("c3").await;
    assert_eq!(result.payload["ok"], false);
    let blob = h.blob(&result);
    assert!(
        blob.contains("Closest line (5): fn two() -> i32 {"),
        "{blob}"
    );
    h.close().await;
}

#[tokio::test]
async fn write_file_notes_unread_overwrites_and_keeps_the_diff() {
    let h = Harness::new();
    let w = h
        .run(
            "w1",
            "write_file",
            json!({"path": "src/x.rs", "content": "fn one() -> i32 {\n    1\n}\n"}),
        )
        .await;
    assert_eq!(w.kind, ToolResultKind::Ok);
    assert_eq!(w.summary, "wrote src/x.rs (+0 −4)");
    let (_, result) = h.pair("w1").await;
    let blob = h.blob(&result);
    assert!(
        blob.contains("note: overwrote a file you had not read"),
        "{blob}"
    );
    assert!(blob.contains("-fn two() -> i32 {\n"), "{blob}");
    assert_eq!(result.payload["metadata"]["created"], false);
    assert_eq!(
        result.payload["metadata"]["note"],
        "note: overwrote a file you had not read"
    );

    let w = h
        .run(
            "w2",
            "write_file",
            json!({"path": "docs/new.md", "content": "# New\n"}),
        )
        .await;
    assert_eq!(w.summary, "wrote docs/new.md (new, 1 line)");
    assert_eq!(h.file("docs/new.md"), "# New\n");
    let (_, result) = h.pair("w2").await;
    assert_eq!(result.payload["metadata"]["created"], true);
    assert!(!h.blob(&result).contains("---"));
    h.close().await;
}

#[tokio::test]
async fn the_seen_set_follows_the_agent_across_steps() {
    let h = Harness::new();
    // Step 1 reads; step 2 (a new executor with the same set) edits.
    h.run("s1", "read_file", json!({"path": "src/x.rs"})).await;
    let step2 = h
        .executor()
        .execute_one(ToolCall::new(
            "s2",
            "edit_file",
            json!({"path": "src/x.rs", "old_string": "fn one", "new_string": "fn uno"}),
        ))
        .await;
    assert_eq!(step2.kind, ToolResultKind::Ok);
    let (text, _) = text_of(&step2.block);
    assert!(!text.contains("note:"), "{text}");

    // A fresh executor without the set does not know the file.
    let stranger = Executor::new(
        &h.registry,
        &AllowAll,
        &h.writer,
        &h.config,
        h.at.clone(),
        CancellationToken::new(),
    )
    .with_workspace(Some(Arc::clone(&h.workspace)))
    .execute_one(ToolCall::new(
        "s3",
        "edit_file",
        json!({"path": "src/x.rs", "old_string": "fn uno", "new_string": "fn one"}),
    ))
    .await;
    let (text, _) = text_of(&stranger.block);
    assert!(
        text.contains("note: overwrote a file you had not read"),
        "{text}"
    );
    h.close().await;
}

#[tokio::test]
async fn paths_outside_the_workspace_are_denied_results() {
    let h = Harness::new();
    let outside = h.repo.path().parent().unwrap().join("elsewhere.txt");
    let out = h
        .run(
            "d1",
            "write_file",
            json!({"path": outside.to_string_lossy(), "content": "x"}),
        )
        .await;
    assert_eq!(out.kind, ToolResultKind::Denied);
    assert!(!out.ok);
    let (text, is_error) = text_of(&out.block);
    assert!(is_error);
    assert!(text.contains("is outside the workspace"), "{text}");
    assert!(!outside.exists());
    let (call, result) = h.pair("d1").await;
    assert_eq!(call.payload["name"], "write_file");
    assert_eq!(result.payload["kind"], "denied");
    assert!(result.blob_id.is_none());

    let out = h
        .run("d2", "read_file", json!({"path": "../../etc/passwd"}))
        .await;
    assert_eq!(out.kind, ToolResultKind::Denied);

    // Reads, listings and globs go through the same sandbox.
    let out = h.run("d3", "list_dir", json!({"path": ".."})).await;
    assert_eq!(out.kind, ToolResultKind::Denied);
    let out = h
        .run("d4", "glob", json!({"pattern": "*", "path": "../.."}))
        .await;
    assert_eq!(out.kind, ToolResultKind::Denied);
    h.close().await;
}

#[tokio::test]
async fn read_only_file_tools_run_together_and_writes_after() {
    let h = Harness::new();
    let results = h
        .executor()
        .execute_all(vec![
            ToolCall::new("b1", "list_dir", json!({"depth": 2})),
            ToolCall::new(
                "b2",
                "write_file",
                json!({"path": "src/y.rs", "content": "// y\n"}),
            ),
            ToolCall::new("b3", "glob", json!({"pattern": "*.rs"})),
            ToolCall::new("b4", "read_file", json!({"path": "src/y.rs"})),
        ])
        .await;
    let kinds: Vec<_> = results
        .iter()
        .map(|r| (r.call_id.as_str(), r.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("b1", ToolResultKind::Ok),
            ("b2", ToolResultKind::Ok),
            ("b3", ToolResultKind::Ok),
            // The read ran before the write (read-only calls go first).
            ("b4", ToolResultKind::Error),
        ]
    );
    let (glob, _) = text_of(&results[2].block);
    assert_eq!(glob, "src/x.rs\n");
    assert!(Path::new(&h.repo.path().join("src/y.rs")).exists());
    h.close().await;
}
