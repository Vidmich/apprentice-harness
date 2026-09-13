//! Goldens for the search tool's three modes, context, truncation and
//! errors, plus the behaviours the task pins down: ignore rules and the
//! `glob` filter, CRLF files, multiline matching, determinism.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::tools::{SeenFiles, ToolContent, ToolEnv, ToolValidator};
use crate::trace::{AgentId, SessionId};
use crate::workspace::Workspace;

struct Fixture {
    _dir: tempfile::TempDir,
    ws: Arc<Workspace>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let w = |rel: &str, bytes: &[u8]| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        };
        w("README.md", b"# Sample\n\nHello world.\nhello again\n");
        w(
            "src/main.rs",
            b"fn main() {\n    println!(\"hello\");\n    helper();\n}\n\nfn helper() {\n    // TODO: hello\n}\n",
        );
        w(
            "src/lib.rs",
            b"pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn sub(a: i32, b: i32) -> i32 {\n    a - b\n}\n",
        );
        w("src/util/x.rs", b"pub fn x() {}\n");
        w(
            "web/app.ts",
            b"export function hello() {\n  return 'hello';\n}\n",
        );
        w("crlf.txt", b"hello\r\nworld\r\nhello world\r\n");
        w("multi.txt", b"start\nmiddle\nend\n");
        w("latin1.txt", b"caf\xE9 hello\n");
        w("blob.dat", b"hello\0\0\0binary\n");
        w(".gitignore", b"*.log\n");
        w("debug.log", b"hello ignored\n");
        w("node_modules/pkg/index.js", b"hello = 1;\n");
        w(".harness/ignore", b"generated/\n");
        w("generated/out.txt", b"hello generated\n");
        let mut long = String::from("hello ");
        long.push_str(&"x".repeat(500));
        long.push('\n');
        w("long.txt", long.as_bytes());
        let ws = Arc::new(Workspace::open(root).unwrap());
        Self { _dir: dir, ws }
    }

    fn ctx(&self) -> ToolContext {
        let (progress, _rx) = mpsc::channel(1);
        ToolContext {
            workspace: Some(Arc::clone(&self.ws)),
            session_id: SessionId::generate(),
            agent_id: AgentId::generate(),
            call_id: "t1".into(),
            env: ToolEnv::default(),
            config: Arc::default(),
            seen: Arc::new(SeenFiles::new()),
            progress,
        }
    }

    async fn call(&self, input: Value) -> Result<ToolOutput, ToolError> {
        let spec = Grep.spec();
        ToolValidator::compile(&spec.input_schema)
            .unwrap()
            .validate(&input)?;
        Grep.call(&self.ctx(), input, CancellationToken::new())
            .await
    }

    async fn run(&self, input: Value) -> ToolOutput {
        self.call(input).await.unwrap()
    }
}

/// Everything the mentor and the trace get, for the goldens.
fn show(out: &ToolOutput) -> String {
    let nl = if text(out).ends_with('\n') { "" } else { "\n" };
    format!(
        "{}\n{}{nl}---\nsummary: {}\nmetadata: {}",
        if out.is_error { "[error]" } else { "[ok]" },
        text(out),
        out.summary,
        serde_json::to_string(&out.metadata).unwrap()
    )
}

fn text(out: &ToolOutput) -> &str {
    match &out.content {
        ToolContent::Text(t) => t,
        other => panic!("not text: {other:?}"),
    }
}

fn paths(out: &ToolOutput) -> Vec<&str> {
    text(out)
        .lines()
        .filter(|l| !l.starts_with('['))
        .map(|l| l.split(':').next().unwrap())
        .collect()
}

#[tokio::test]
async fn content_mode_golden() {
    let f = Fixture::new();
    let out = f.run(json!({"pattern": "hello"})).await;
    insta::assert_snapshot!("grep_content", show(&out));
    // Byte order of the workspace-relative paths, then line numbers.
    let shown = paths(&out);
    let mut sorted = shown.clone();
    sorted.sort_unstable();
    assert_eq!(shown, sorted);
    // The same again: nothing depends on thread timing.
    let again = f.run(json!({"pattern": "hello"})).await;
    assert_eq!(text(&again), text(&out));
}

#[tokio::test]
async fn context_lines_and_group_breaks_golden() {
    let f = Fixture::new();
    let out = f
        .run(json!({"pattern": "fn ", "path": "src", "context": 1}))
        .await;
    insta::assert_snapshot!("grep_context", show(&out));
    let t = text(&out);
    assert!(
        t.contains("src/lib.rs-2-    a + b\n--\nsrc/lib.rs-4-\n"),
        "{t}"
    );
    assert!(
        t.contains("src/lib.rs-6-    a - b\n--\nsrc/main.rs:1:1:fn main"),
        "{t}"
    );
}

#[tokio::test]
async fn files_and_count_modes_golden() {
    let f = Fixture::new();
    let files = f.run(json!({"pattern": "hello", "mode": "files"})).await;
    let count = f.run(json!({"pattern": "hello", "mode": "count"})).await;
    let mut both = show(&files);
    both.push_str("\n=====\n");
    both.push_str(&show(&count));
    insta::assert_snapshot!("grep_files_count", both);
}

#[tokio::test]
async fn max_results_truncates_every_mode_golden() {
    let f = Fixture::new();
    let mut all = String::new();
    for mode in ["content", "files", "count"] {
        let out = f
            .run(json!({"pattern": "hello", "mode": mode, "max_results": 2}))
            .await;
        let _ = writeln!(all, "==== {mode}\n{}", show(&out));
        assert!(
            text(&out).ends_with("[limit reached; narrow the pattern or path]\n"),
            "{}",
            text(&out)
        );
        assert_eq!(out.metadata["truncated"], true);
    }
    insta::assert_snapshot!("grep_truncated", all);
}

#[tokio::test]
async fn ignore_rules_apply_and_glob_narrows() {
    let f = Fixture::new();
    let out = f.run(json!({"pattern": "hello", "mode": "files"})).await;
    let found = paths(&out);
    for hidden in [
        "debug.log",
        "node_modules/pkg/index.js",
        "generated/out.txt",
        "blob.dat",
    ] {
        assert!(!found.contains(&hidden), "{hidden} in {found:?}");
    }
    assert!(found.contains(&"src/main.rs"));

    let rs = f
        .run(json!({"pattern": "hello", "mode": "files", "glob": "*.rs"}))
        .await;
    assert_eq!(paths(&rs), ["src/main.rs"]);

    // A path glob is relative to `path`.
    let deep = f
        .run(json!({"pattern": "fn", "mode": "files", "path": "src", "glob": "util/*.rs"}))
        .await;
    assert_eq!(paths(&deep), ["src/util/x.rs"]);
    let none = f
        .run(json!({"pattern": "fn", "mode": "files", "glob": "util/*.rs"}))
        .await;
    assert_eq!(text(&none), "[no matches]\n");
    assert_eq!(none.summary, "grep \"fn\" \u{2192} 0 files");

    // Naming an ignored file directly searches it.
    let direct = f
        .run(json!({"pattern": "hello", "path": "debug.log"}))
        .await;
    assert_eq!(
        text(&direct),
        "debug.log:1:1:hello ignored\n[1 match in 1 file]\n"
    );
    assert_eq!(direct.metadata["searched"], 1);
}

#[tokio::test]
async fn crlf_files_match_anchors_and_show_no_cr() {
    let f = Fixture::new();
    let out = f
        .run(json!({"pattern": "world$", "path": "crlf.txt"}))
        .await;
    assert_eq!(
        text(&out),
        "crlf.txt:2:1:world\ncrlf.txt:3:7:hello world\n[2 matches in 1 file]\n"
    );
    assert!(!text(&out).contains('\r'));
    let ci = f
        .run(json!({"pattern": "^HELLO", "case_insensitive": true, "mode": "count"}))
        .await;
    assert!(text(&ci).contains("crlf.txt: 2\n"), "{}", text(&ci));
    assert!(text(&ci).contains("README.md: 2\n"), "{}", text(&ci));
}

#[tokio::test]
async fn multiline_spans_lines_only_when_asked() {
    let f = Fixture::new();
    let plain = f
        .run(json!({"pattern": "start\\nmiddle", "path": "multi.txt"}))
        .await;
    assert!(plain.is_error, "{}", text(&plain));
    assert!(
        text(&plain).starts_with("invalid regex: "),
        "{}",
        text(&plain)
    );

    let dot = f
        .run(json!({"pattern": "start.*end", "path": "multi.txt"}))
        .await;
    assert_eq!(text(&dot), "[no matches]\n");

    let multi = f
        .run(json!({"pattern": "start\\nmiddle", "path": "multi.txt", "multiline": true}))
        .await;
    assert_eq!(
        text(&multi),
        "multi.txt:1:1:start\nmulti.txt:2:1:middle\n[1 match in 1 file]\n"
    );
    let dotall = f
        .run(json!({"pattern": "art.*mid", "multiline": true, "mode": "count"}))
        .await;
    assert_eq!(text(&dotall), "multi.txt: 1\n[1 match in 1 file]\n");
}

#[tokio::test]
async fn long_and_undecodable_lines_are_cleaned() {
    let f = Fixture::new();
    let out = f.run(json!({"pattern": "hello", "path": "long.txt"})).await;
    let line = text(&out).lines().next().unwrap();
    assert_eq!(
        line.chars().count(),
        "long.txt:1:1:".len() + GREP_LINE_CHARS + 1
    );
    assert!(line.ends_with('\u{2026}'));
    let out = f.run(json!({"pattern": "caf", "path": "latin1.txt"})).await;
    assert_eq!(
        text(&out),
        "latin1.txt:1:1:caf\u{FFFD} hello\n[1 match in 1 file]\n"
    );
}

#[tokio::test]
async fn error_results_golden() {
    let f = Fixture::new();
    let mut all = String::new();
    for input in [
        json!({"pattern": "a("}),
        json!({"pattern": "a\\nb"}),
        json!({"pattern": "x", "path": "nope"}),
        json!({"pattern": "x", "glob": "["}),
    ] {
        let _ = writeln!(all, "==== {input}");
        match f.call(input).await {
            Ok(out) => {
                assert!(out.is_error);
                let _ = writeln!(all, "{}", show(&out));
            }
            Err(e) => {
                let _ = writeln!(all, "Err({e:?})");
            }
        }
    }
    insta::assert_snapshot!("grep_errors", all);

    let outside = f
        .call(json!({"pattern": "x", "path": "../elsewhere"}))
        .await;
    assert!(matches!(outside, Err(ToolError::Denied(_))), "{outside:?}");
}

#[tokio::test]
async fn a_cancelled_search_is_reported_as_cancelled() {
    let f = Fixture::new();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let r = Grep
        .call(&f.ctx(), json!({"pattern": "hello"}), cancel)
        .await;
    assert!(matches!(r, Err(ToolError::Cancelled)), "{r:?}");
}

#[test]
fn grep_tool_spec_golden() {
    let spec = Grep.spec();
    ToolValidator::compile(&spec.input_schema).unwrap();
    insta::assert_json_snapshot!("grep_tool_spec", spec);
}

/// The acceptance bar: a rare token over 100k files in under a second
/// (warm cache). On Windows opening 100k files alone takes longer than
/// that (the parallel read of every file is measured here as the
/// floor), so there the bar is "at most one second above the floor".
/// Not part of `just check`: it builds the tree (minutes on Windows)
/// unless `HARNESS_GREP_BENCH_DIR` names a directory to build it in
/// once and reuse. Run it with
/// `cargo test --release -p apprentice-core grep -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "builds a 100k-file tree; run by hand"]
async fn a_rare_token_in_100k_files_takes_under_a_second() {
    let keep = std::env::var_os("HARNESS_GREP_BENCH_DIR").map(std::path::PathBuf::from);
    let tmp = tempfile::tempdir().unwrap();
    let root = keep.clone().unwrap_or_else(|| tmp.path().to_path_buf());
    if !root.join("d099/f0999.rs").is_file() {
        for d in 0..100 {
            let sub = root.join(format!("d{d:03}"));
            std::fs::create_dir_all(&sub).unwrap();
            for i in 0..1000 {
                let body = format!("fn f{i}() {{\n    let value = {i} * {d};\n    value + 1\n}}\n");
                std::fs::write(sub.join(format!("f{i:04}.rs")), body).unwrap();
            }
        }
        std::fs::write(root.join("d050/f0500.rs"), "fn f() { NEEDLE_7f3a }\n").unwrap();
    }
    let f = Fixture {
        ws: Arc::new(Workspace::open(&root).unwrap()),
        _dir: tmp,
    };
    // Warm the cache once, then time it.
    f.run(json!({"pattern": "NEEDLE_7f3a"})).await;
    let start = Instant::now();
    let out = f.run(json!({"pattern": "NEEDLE_7f3a"})).await;
    let took = start.elapsed();
    // The floor: the same parallel walk, reading every file.
    let start = Instant::now();
    let read = std::sync::atomic::AtomicUsize::new(0);
    f.ws.ignore_rules()
        .walk_builder_at(&root, None)
        .build_parallel()
        .run(|| {
            Box::new(|e| {
                if let Ok(e) = e
                    && e.file_type().is_some_and(|t| t.is_file())
                    && std::fs::read(e.path()).is_ok()
                {
                    read.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                ignore::WalkState::Continue
            })
        });
    let floor = start.elapsed();
    eprintln!(
        "100k files: search {took:?}, walk + read of {} files {floor:?}",
        read.load(std::sync::atomic::Ordering::Relaxed)
    );
    assert_eq!(out.metadata["matches"], 1);
    assert_eq!(out.metadata["searched"], 100_000);
    assert!(
        took < floor + Duration::from_secs(1),
        "{took:?} vs floor {floor:?}"
    );
    if !cfg!(windows) {
        assert!(took.as_secs_f64() < 1.0, "{took:?}");
    }
}
