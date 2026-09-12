//! Goldens for the file tools' output formats and the behaviours the
//! task pins down (paging, truncation, binary and encoding handling,
//! CRLF/BOM preservation, edit failures, listing and glob caps).

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::tools::{SeenFiles, Tool, ToolContext, ToolEnv, ToolError, ToolOutput, ToolValidator};
use crate::trace::{AgentId, SessionId};
use crate::workspace::Workspace;

struct Fixture {
    dir: tempfile::TempDir,
    ws: Arc<Workspace>,
    seen: Arc<SeenFiles>,
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
        w("README.md", b"# Sample\n\nHello.\n");
        w("src/main.rs", b"fn main() {\n    println!(\"hi\");\n}\n");
        w(
            "src/lib.rs",
            b"pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn sub(a: i32, b: i32) -> i32 {\n    a - b\n}\n",
        );
        w("src/util/mod.rs", b"pub mod x;\n");
        w("src/util/x.rs", b"pub fn x() {}\n");
        w("crlf.txt", b"one\r\ntwo\r\nthree\r\n");
        w("bom.txt", b"\xEF\xBB\xBFalpha\nbeta\n");
        w("latin1.txt", b"caf\xE9\n");
        w("utf16.txt", b"\xFF\xFEh\0i\0\n\0");
        w("empty.txt", b"");
        w("bin/logo.png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01");
        w(".gitignore", b"*.log\n");
        w("debug.log", b"ignored\n");
        w("node_modules/pkg/index.js", b"module.exports = 1;\n");
        w("target/debug/out", b"x");
        w(".harness/HARNESS.md", b"# Instructions\n");
        w(".harness/ignore", b"");
        std::fs::create_dir_all(root.join("emptydir")).unwrap();
        // One old mtime for everything, so `glob` orders by path unless
        // a test sets mtimes itself.
        for entry in walkdir(root) {
            set_mtime(&entry, 1000);
        }
        let ws = Arc::new(Workspace::open(root).unwrap());
        Self {
            dir,
            ws,
            seen: Arc::new(SeenFiles::new()),
        }
    }

    fn ctx(&self) -> ToolContext {
        let (progress, _rx) = mpsc::channel(1);
        ToolContext {
            workspace: Some(Arc::clone(&self.ws)),
            session_id: SessionId::generate(),
            agent_id: AgentId::generate(),
            call_id: "t1".into(),
            env: ToolEnv::default(),
            seen: Arc::clone(&self.seen),
            progress,
        }
    }

    async fn call(&self, tool: &dyn Tool, input: Value) -> Result<ToolOutput, ToolError> {
        let spec = tool.spec();
        ToolValidator::compile(&spec.input_schema)
            .unwrap()
            .validate(&input)?;
        tool.call(&self.ctx(), input, CancellationToken::new())
            .await
    }

    async fn run(&self, tool: &dyn Tool, input: Value) -> ToolOutput {
        self.call(tool, input).await.unwrap()
    }

    fn path(&self, rel: &str) -> std::path::PathBuf {
        self.dir.path().join(rel)
    }

    fn bytes(&self, rel: &str) -> Vec<u8> {
        std::fs::read(self.path(rel)).unwrap()
    }
}

/// Everything the mentor and the trace get, for the goldens.
fn show(out: &ToolOutput) -> String {
    let text = match &out.content {
        crate::tools::ToolContent::Text(t) => t.clone(),
        other => format!("{other:?}"),
    };
    let nl = if text.ends_with('\n') { "" } else { "\n" };
    format!(
        "{}\n{text}{nl}---\nsummary: {}\nmetadata: {}",
        if out.is_error { "[error]" } else { "[ok]" },
        out.summary,
        serde_json::to_string(&out.metadata).unwrap()
    )
}

fn text(out: &ToolOutput) -> &str {
    match &out.content {
        crate::tools::ToolContent::Text(t) => t,
        other => panic!("not text: {other:?}"),
    }
}

fn walkdir(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walkdir(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// `"<prefix>1\n<prefix>2\n..."` up to `n`.
fn lines_named(prefix: &str, n: usize) -> String {
    let mut s = String::new();
    for i in 1..=n {
        let _ = writeln!(s, "{prefix}{i}");
    }
    s
}

fn set_mtime(path: &Path, secs_ago: u64) {
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(secs_ago))
        .unwrap();
}

// ------------------------------------------------------------ read_file

#[tokio::test]
async fn read_file_numbers_lines() {
    let f = Fixture::new();
    let out = f.run(&ReadFile, json!({"path": "src/main.rs"})).await;
    insta::assert_snapshot!("read_basic", show(&out));
    assert!(!out.is_error);
    assert_eq!(out.metadata["lines"], 3);
    assert_eq!(out.metadata["language"], "rust");
    assert!(f.seen.contains(&f.ws.resolve("src/main.rs").unwrap()));
    // Native separators and a leading `./` are fine too.
    let out = f.run(&ReadFile, json!({"path": "./src\\main.rs"})).await;
    assert!(!out.is_error, "{}", text(&out));
}

#[tokio::test]
async fn read_file_pages_with_offset_and_limit() {
    let f = Fixture::new();
    let lines = lines_named("line ", 10);
    std::fs::write(f.path("ten.txt"), lines).unwrap();
    let out = f
        .run(
            &ReadFile,
            json!({"path": "ten.txt", "offset": 4, "limit": 3}),
        )
        .await;
    insta::assert_snapshot!("read_paged", show(&out));
    let out = f
        .run(
            &ReadFile,
            json!({"path": "ten.txt", "offset": 9, "limit": 50}),
        )
        .await;
    assert_eq!(text(&out), "9\tline 9\n10\tline 10\n");
    assert_eq!(out.metadata["truncated"], false);
    assert_eq!(out.summary, "read ten.txt lines 9–10 of 10");
    let out = f
        .run(&ReadFile, json!({"path": "ten.txt", "offset": 11}))
        .await;
    assert!(out.is_error);
    assert_eq!(
        text(&out),
        "offset 11 is past the end of `ten.txt` (10 lines)"
    );
}

#[tokio::test]
async fn read_file_truncates_at_the_line_and_byte_caps() {
    let f = Fixture::new();
    let many = lines_named("l", 2500);
    std::fs::write(f.path("many.txt"), many).unwrap();
    let out = f.run(&ReadFile, json!({"path": "many.txt"})).await;
    let t = text(&out);
    assert!(t.starts_with("1\tl1\n"));
    assert!(t.contains(
        "\n2000\tl2000\n[truncated: showing lines 1–2000 of 2500; call again with offset 2001]\n"
    ));
    assert!(!t.contains("\n2001\t"));
    assert_eq!(out.metadata["truncated"], true);
    assert_eq!(out.metadata["end"], 2000);
    assert_eq!(out.summary, "read many.txt lines 1–2000 of 2500");

    let wide = format!("{}\n", "x".repeat(999)).repeat(300);
    std::fs::write(f.path("wide.txt"), wide).unwrap();
    let out = f.run(&ReadFile, json!({"path": "wide.txt"})).await;
    // 1000 bytes of content per line, 200 KiB budget.
    let shown = READ_MAX_BYTES / 1000;
    assert_eq!(out.metadata["end"], shown);
    assert!(text(&out).ends_with(&format!(
        "[truncated: showing lines 1–{shown} of 300; call again with offset {}]\n",
        shown + 1
    )));
    // A single line longer than the budget still comes out.
    std::fs::write(f.path("huge.txt"), "y".repeat(READ_MAX_BYTES + 10)).unwrap();
    let out = f.run(&ReadFile, json!({"path": "huge.txt"})).await;
    assert_eq!(out.metadata["end"], 1);
    assert_eq!(out.metadata["truncated"], false);
}

#[tokio::test]
async fn read_file_error_results() {
    let f = Fixture::new();
    let mut all = String::new();
    for path in ["bin/logo.png", "missing.txt", "src", "empty.txt"] {
        let out = f.run(&ReadFile, json!({"path": path})).await;
        let _ = write!(all, "# {path}\n{}\n\n", show(&out));
    }
    insta::assert_snapshot!("read_errors", all);
}

#[tokio::test]
async fn read_file_decodes_lossy_utf16_bom_and_crlf() {
    let f = Fixture::new();
    let mut all = String::new();
    for path in ["latin1.txt", "utf16.txt", "bom.txt", "crlf.txt"] {
        let out = f.run(&ReadFile, json!({"path": path})).await;
        assert!(!out.is_error, "{path}");
        let _ = write!(all, "# {path}\n{}\n\n", show(&out));
    }
    insta::assert_snapshot!("read_encodings", all);
}

// ----------------------------------------------------------- write_file

#[tokio::test]
async fn write_file_creates_and_overwrites() {
    let f = Fixture::new();
    let out = f
        .run(
            &WriteFile,
            json!({"path": "new/dir/hello.txt", "content": "hello\nworld\n"}),
        )
        .await;
    insta::assert_snapshot!("write_new", show(&out));
    assert_eq!(f.bytes("new/dir/hello.txt"), b"hello\nworld\n");
    assert_eq!(out.metadata["created"], true);

    // Overwriting a file that was never read is noted, with the diff.
    let out = f
        .run(
            &WriteFile,
            json!({"path": "README.md", "content": "# Sample\n\nGoodbye.\nBye.\n"}),
        )
        .await;
    insta::assert_snapshot!("write_overwrite_unread", show(&out));
    assert_eq!(out.summary, "wrote README.md (+2 −1)");

    // After a read (the write above recorded the content) no note.
    let out = f
        .run(
            &WriteFile,
            json!({"path": "README.md", "content": "# Sample\n\nGoodbye.\n"}),
        )
        .await;
    assert!(!text(&out).contains("note:"), "{}", text(&out));
    assert!(out.metadata.get("note").is_none());

    // Changed behind the mentor's back.
    std::fs::write(f.path("README.md"), "# Someone else\n").unwrap();
    let out = f
        .run(
            &WriteFile,
            json!({"path": "README.md", "content": "# Mine\n"}),
        )
        .await;
    assert!(text(&out).contains("note: the file changed on disk since you read it"));
    assert_eq!(
        out.metadata["note"],
        "note: the file changed on disk since you read it"
    );

    // Same content: no diff to show.
    let out = f
        .run(
            &WriteFile,
            json!({"path": "README.md", "content": "# Mine\n"}),
        )
        .await;
    assert!(text(&out).contains("(content unchanged)"));
    assert_eq!(out.summary, "wrote README.md (+0 −0)");

    // Replacing a binary file.
    f.run(&ReadFile, json!({"path": "bin/logo.png"})).await;
    let out = f
        .run(
            &WriteFile,
            json!({"path": "bin/logo.png", "content": "text now\n"}),
        )
        .await;
    assert!(text(&out).contains("(previous content was binary; no diff)"));
    assert_eq!(out.summary, "wrote bin/logo.png (+1 −0)");

    let out = f
        .run(&WriteFile, json!({"path": "src", "content": "x"}))
        .await;
    assert!(out.is_error);
    assert_eq!(text(&out), "`src` is a directory");
}

#[tokio::test]
async fn write_file_refreshes_the_index_for_new_files() {
    let f = Fixture::new();
    let before = f.ws.index();
    assert!(before.get("fresh.txt").is_none());
    f.run(&WriteFile, json!({"path": "fresh.txt", "content": "x"}))
        .await;
    let after = f.ws.index();
    assert!(after.get("fresh.txt").is_some());
    let out = f.run(&Glob, json!({"pattern": "fresh.*"})).await;
    assert_eq!(text(&out), "fresh.txt\n");
}

// ------------------------------------------------------------ edit_file

#[tokio::test]
async fn edit_file_replaces_one_occurrence() {
    let f = Fixture::new();
    f.run(&ReadFile, json!({"path": "src/lib.rs"})).await;
    let out = f
        .run(
            &EditFile,
            json!({
                "path": "src/lib.rs",
                "old_string": "    a + b\n",
                "new_string": "    // sum\n    a + b\n"
            }),
        )
        .await;
    insta::assert_snapshot!("edit_single", show(&out));
    assert_eq!(out.summary, "edited src/lib.rs (+1 −0)");
    assert_eq!(
        f.bytes("src/lib.rs"),
        b"pub fn add(a: i32, b: i32) -> i32 {\n    // sum\n    a + b\n}\n\npub fn sub(a: i32, b: i32) -> i32 {\n    a - b\n}\n"
    );

    // Deleting text, and a note when the file was not read first.
    let f = Fixture::new();
    let out = f
        .run(
            &EditFile,
            json!({"path": "src/main.rs", "old_string": "    println!(\"hi\");\n", "new_string": ""}),
        )
        .await;
    assert_eq!(
        text(&out).lines().nth(1),
        Some("note: overwrote a file you had not read")
    );
    assert_eq!(f.bytes("src/main.rs"), b"fn main() {\n}\n");
}

#[tokio::test]
async fn edit_file_replace_all() {
    let f = Fixture::new();
    let out = f
        .run(
            &EditFile,
            json!({"path": "src/lib.rs", "old_string": "i32", "new_string": "i64", "replace_all": true}),
        )
        .await;
    insta::assert_snapshot!("edit_replace_all", show(&out));
    assert_eq!(out.metadata["replacements"], 6);
    assert!(!f.bytes("src/lib.rs").windows(3).any(|w| w == b"i32"));
}

#[tokio::test]
async fn edit_file_no_match_and_ambiguity_errors() {
    let f = Fixture::new();
    let mut all = String::new();
    let cases = [
        json!({"path": "src/lib.rs", "old_string": "    a+b\n", "new_string": "x"}),
        json!({"path": "src/lib.rs", "old_string": "zzzz qqqq", "new_string": "x"}),
        json!({"path": "src/lib.rs", "old_string": "i32", "new_string": "i64"}),
        json!({"path": "src/lib.rs", "old_string": "same", "new_string": "same"}),
        json!({"path": "nope.rs", "old_string": "a", "new_string": "b"}),
        json!({"path": "bin/logo.png", "old_string": "a", "new_string": "b"}),
        json!({"path": "latin1.txt", "old_string": "caf", "new_string": "bar"}),
        json!({"path": "utf16.txt", "old_string": "hi", "new_string": "yo"}),
    ];
    for input in cases {
        let out = f.run(&EditFile, input.clone()).await;
        assert!(out.is_error, "{input}");
        let _ = write!(all, "# {input}\n{}\n\n", text(&out));
    }
    insta::assert_snapshot!("edit_errors", all);
    // Nothing was written.
    assert_eq!(f.bytes("latin1.txt"), b"caf\xE9\n");
}

#[tokio::test]
async fn edit_file_preserves_crlf_and_bom() {
    let f = Fixture::new();
    // The mentor quotes LF (that is what read_file showed).
    let out = f
        .run(
            &EditFile,
            json!({"path": "crlf.txt", "old_string": "two\n", "new_string": "2\n2b\n"}),
        )
        .await;
    assert!(!out.is_error, "{}", text(&out));
    assert_eq!(f.bytes("crlf.txt"), b"one\r\n2\r\n2b\r\nthree\r\n");
    assert_eq!(out.metadata["line_endings"], "crlf");
    assert_eq!(out.metadata["added"], 2);
    assert_eq!(out.metadata["removed"], 1);
    // The diff the mentor sees has no carriage returns.
    assert!(!text(&out).contains('\r'));

    // Quoting CRLF explicitly works as well, and CRLF in new_string does
    // not double up.
    let out = f
        .run(
            &EditFile,
            json!({"path": "crlf.txt", "old_string": "2b\r\n", "new_string": "2c\r\n"}),
        )
        .await;
    assert!(!out.is_error, "{}", text(&out));
    assert_eq!(f.bytes("crlf.txt"), b"one\r\n2\r\n2c\r\nthree\r\n");
    let out = f
        .run(
            &EditFile,
            json!({"path": "crlf.txt", "old_string": "2c\n", "new_string": "2d\r\n"}),
        )
        .await;
    assert!(!out.is_error, "{}", text(&out));
    assert_eq!(f.bytes("crlf.txt"), b"one\r\n2\r\n2d\r\nthree\r\n");

    // A mixed file is edited byte for byte.
    std::fs::write(f.path("mixed.txt"), b"a\r\nb\nc\r\n").unwrap();
    let out = f
        .run(
            &EditFile,
            json!({"path": "mixed.txt", "old_string": "b\n", "new_string": "B\n"}),
        )
        .await;
    assert!(!out.is_error, "{}", text(&out));
    assert_eq!(f.bytes("mixed.txt"), b"a\r\nB\nc\r\n");
    assert_eq!(out.metadata["line_endings"], "mixed");

    let out = f
        .run(
            &EditFile,
            json!({"path": "bom.txt", "old_string": "alpha", "new_string": "ALPHA"}),
        )
        .await;
    assert!(!out.is_error, "{}", text(&out));
    assert_eq!(f.bytes("bom.txt"), b"\xEF\xBB\xBFALPHA\nbeta\n");
    assert_eq!(out.metadata["bom"], true);
}

// ------------------------------------------------------------- list_dir

#[tokio::test]
async fn list_dir_trees() {
    let f = Fixture::new();
    let mut all = String::new();
    for input in [
        json!({}),
        json!({"path": "src", "depth": 2}),
        json!({"path": ".", "depth": 4}),
        json!({"path": "emptydir"}),
        json!({"path": "target"}),
    ] {
        let out = f.run(&ListDir, input.clone()).await;
        assert!(!out.is_error, "{input}: {}", text(&out));
        let _ = write!(all, "# {input}\n{}\n\n", show(&out));
    }
    insta::assert_snapshot!("list_trees", all);

    let out = f.run(&ListDir, json!({"path": "README.md"})).await;
    assert!(out.is_error);
    assert_eq!(text(&out), "`README.md` is a file; use read_file");
    let out = f.run(&ListDir, json!({"path": "nope"})).await;
    assert!(out.is_error);
    assert_eq!(text(&out), "`nope` does not exist");
    let err = f.call(&ListDir, json!({"depth": 9})).await.unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));
}

#[tokio::test]
async fn list_dir_caps_the_entries() {
    let f = Fixture::new();
    let big = f.path("big");
    std::fs::create_dir(&big).unwrap();
    for i in 0..(LIST_MAX_ENTRIES + 100) {
        std::fs::write(big.join(format!("f{i:05}")), b"").unwrap();
    }
    let out = f.run(&ListDir, json!({"path": "big"})).await;
    let t = text(&out);
    assert!(t.starts_with("f00000\t0\n"));
    assert!(t.ends_with("[+100 more]\n"));
    assert_eq!(t.lines().count(), LIST_MAX_ENTRIES + 1);
    assert_eq!(out.metadata["entries"], LIST_MAX_ENTRIES + 100);
    assert_eq!(out.metadata["truncated"], true);
    assert_eq!(out.summary, "listed big/ (2100 entries)");
}

// ----------------------------------------------------------------- glob

#[tokio::test]
async fn glob_matches_by_name_or_path_newest_first() {
    let f = Fixture::new();
    set_mtime(&f.path("src/main.rs"), 10);
    set_mtime(&f.path("src/lib.rs"), 30);
    set_mtime(&f.path("src/util/mod.rs"), 20);
    set_mtime(&f.path("src/util/x.rs"), 40);
    let mut all = String::new();
    for input in [
        json!({"pattern": "*.rs"}),
        json!({"pattern": "src/*.rs"}),
        json!({"pattern": "**/*.rs", "path": "src/util"}),
        json!({"pattern": "*.{md,txt}"}),
        json!({"pattern": "*.log"}),
        json!({"pattern": "index.js"}),
    ] {
        let out = f.run(&Glob, input.clone()).await;
        assert!(!out.is_error, "{input}: {}", text(&out));
        let _ = write!(all, "# {input}\n{}\n\n", show(&out));
    }
    insta::assert_snapshot!("glob_matches", all);

    let err = f
        .call(&Glob, json!({"pattern": "[unclosed"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)), "{err}");
    let out = f.run(&Glob, json!({"pattern": "*", "path": "nope"})).await;
    assert!(out.is_error);
}

#[tokio::test]
async fn glob_caps_the_results() {
    let f = Fixture::new();
    let big = f.path("big");
    std::fs::create_dir(&big).unwrap();
    for i in 0..(GLOB_MAX_RESULTS + 5) {
        std::fs::write(big.join(format!("g{i:05}.txt")), b"").unwrap();
    }
    let out = f.run(&Glob, json!({"pattern": "g*.txt"})).await;
    let t = text(&out);
    assert_eq!(t.lines().count(), GLOB_MAX_RESULTS + 1);
    assert!(t.ends_with("[truncated: showing 1000 of 1005 matches]\n"));
    assert_eq!(out.summary, "glob g*.txt → 1005 files");
    assert_eq!(out.metadata["truncated"], true);
}

// --------------------------------------------------------------- shared

#[tokio::test]
async fn paths_outside_the_workspace_are_denied() {
    let f = Fixture::new();
    for input in [
        json!({"path": "../etc/passwd"}),
        json!({"path": f.dir.path().parent().unwrap().to_string_lossy()}),
        json!({"path": if cfg!(windows) { r"C:\Windows\win.ini" } else { "/etc/hosts" }}),
    ] {
        let err = f.call(&ReadFile, input.clone()).await.unwrap_err();
        assert!(matches!(err, ToolError::Denied(_)), "{input}: {err}");
        assert!(err.to_string().contains("outside the workspace"), "{err}");
    }
    let err = f
        .call(&WriteFile, json!({"path": "../escape.txt", "content": "x"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied(_)));
    assert!(!f.dir.path().parent().unwrap().join("escape.txt").exists());

    let err = f
        .call(&ReadFile, json!({"path": "a\u{0}b"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)));

    let mut ctx = f.ctx();
    ctx.workspace = None;
    let err = ReadFile
        .call(&ctx, json!({"path": "README.md"}), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Failed(_)));
    assert!(err.to_string().contains("no workspace"));
}

#[test]
fn specs_are_stable() {
    // The descriptions are part of the mentor's cached prefix: a change
    // here is deliberate and invalidates the cache for every session.
    let specs: Vec<_> = file_tools().iter().map(|t| t.spec()).collect();
    assert_eq!(
        specs.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["read_file", "write_file", "edit_file", "list_dir", "glob"]
    );
    insta::assert_json_snapshot!("file_tool_specs", specs);
}
