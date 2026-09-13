//! The system prompt (task M01-09): the `#workspace` block of a fixture
//! workspace against a snapshot, the project instructions (present,
//! oversized, absent), the core's bytes shared across workspaces, the
//! git line on a real repository, and `prompt.show` through
//! `AppState` for a workspace and for a session.

use std::path::Path;
use std::sync::Arc;

use apprentice_api::methods::{PromptShowParams, SessionCreateParams};
use apprentice_core::app::AppState;
use apprentice_core::config::{Config, ConfigLoader, Paths, SecretStoreKind};
use apprentice_core::runtime::prompt::INSTRUCTIONS_MAX;
use apprentice_core::runtime::prompt_show;
use apprentice_core::runtime::{
    Host, MENTOR_SYSTEM_V1, PROMPT_VERSION, WorkspaceContext, assemble, build_system,
};
use apprentice_core::workspace::Workspace;

fn host() -> Host {
    Host {
        os: "windows 11 (26100)".into(),
        shell: "pwsh".into(),
        harness: "0.1.0".into(),
    }
}

/// A small mixed project: rust, typescript, markdown, toml; a
/// `target/` the built-in ignores hide; no git.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (path, text) in [
        ("Cargo.toml", "[package]\nname = \"fixture\"\n"),
        ("README.md", "# fixture\n"),
        ("src/main.rs", "fn main() {}\n"),
        ("src/lib.rs", "pub fn f() {}\n"),
        ("src/util.rs", "pub fn g() {}\n"),
        ("apps/web/index.ts", "export const x = 1;\n"),
        ("apps/web/app.ts", "export const y = 2;\n"),
        ("target/debug/out.rs", "ignored\n"),
        ("notes.txt", "no language\n"),
    ] {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    dir
}

fn instructions(root: &Path, text: &str) {
    std::fs::create_dir_all(root.join(".harness")).unwrap();
    std::fs::write(root.join(".harness/HARNESS.md"), text).unwrap();
}

async fn context(root: &Path) -> WorkspaceContext {
    let ws = Arc::new(Workspace::open(root).unwrap());
    let mut ctx = WorkspaceContext::gather(Some(&ws), host()).await;
    // The temp root differs per run; the snapshot shows a fixed one.
    assert!(
        ctx.root
            .as_deref()
            .unwrap()
            .starts_with(&ws.root_string().replace('\\', "/"))
    );
    ctx.root = Some("C:/src/fixture".into());
    ctx
}

#[tokio::test]
async fn the_workspace_block_of_a_fixture_matches_the_snapshot() {
    let dir = fixture();
    instructions(
        dir.path(),
        "# Fixture rules\n\n- Run `cargo test` after every change.\n- Never touch `apps/web`.\n",
    );
    let ctx = context(dir.path()).await;
    assert_eq!(ctx.git.as_deref(), Some("not a git repository"));
    assert_eq!(
        ctx.languages,
        [
            ("rust".to_owned(), 33),
            ("markdown".to_owned(), 22),
            ("typescript".to_owned(), 22),
            ("text".to_owned(), 11),
            ("toml".to_owned(), 11)
        ],
        "3 rs, 2 md (README, HARNESS.md), 2 ts, 1 txt, 1 toml of 9 indexed files; target/ is ignored"
    );
    assert_eq!(ctx.top_level_more, 0);
    let prompt = assemble(&ctx);
    assert_eq!(prompt.version, PROMPT_VERSION);
    assert_eq!(prompt.blocks[0].text, MENTOR_SYSTEM_V1);
    insta::assert_snapshot!("workspace_block", prompt.blocks[1].text);
}

#[tokio::test]
async fn instructions_are_included_truncated_or_omitted() {
    let dir = fixture();
    // Absent: no section, and the block ends with the listing.
    let without = context(dir.path()).await;
    assert_eq!(without.instructions, None);
    let text = without.render();
    assert!(!text.contains("#instructions"), "{text}");
    assert!(text.ends_with("top-level: apps/, src/, Cargo.toml, README.md, notes.txt\n"));

    // Present: verbatim.
    instructions(dir.path(), "Use tabs.\n");
    let with = context(dir.path()).await;
    let text = with.render();
    assert!(
        text.ends_with("#instructions (from .harness/HARNESS.md)\nUse tabs.\n"),
        "{text}"
    );

    // Oversized: the first 8 KiB and a note.
    let big = "x".repeat(INSTRUCTIONS_MAX + 100);
    instructions(dir.path(), &big);
    let cut = context(dir.path()).await;
    let i = cut.instructions.as_ref().unwrap();
    assert_eq!(i.text.len(), INSTRUCTIONS_MAX);
    assert_eq!(i.truncated_from, Some(INSTRUCTIONS_MAX + 100));
    let text = cut.render();
    assert!(
        text.ends_with(&format!(
            "[truncated: the first {INSTRUCTIONS_MAX} of {} bytes are shown; keep the file under {INSTRUCTIONS_MAX} bytes]\n",
            INSTRUCTIONS_MAX + 100
        )),
        "{}",
        &text[text.len() - 200..]
    );

    // The core block is the same bytes whatever the workspace says.
    let a = assemble(&without);
    let b = assemble(&cut);
    assert_eq!(a.blocks[0], b.blocks[0]);
    assert_ne!(a.blocks[1], b.blocks[1]);
    let c = build_system(None, &Config::default()).await;
    assert_eq!(c.blocks[0], a.blocks[0]);
    assert!(c.blocks[1].text.starts_with("#workspace\nroot: none"));
}

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
async fn the_git_line_names_branch_head_and_counts() {
    let dir = fixture();
    let root = dir.path();
    if !git(root, &["init", "-q", "-b", "main"]) {
        eprintln!("skipping: git is not on PATH");
        return;
    }
    let unborn = context(root).await;
    assert_eq!(
        unborn.git.as_deref(),
        Some("branch main (no commits yet), 9 untracked"),
        "no .gitignore: target/ counts"
    );
    for args in [
        vec!["config", "user.name", "Test User"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["add", "."],
        vec!["commit", "-qm", "init"],
    ] {
        assert!(git(root, &args), "git {args:?}");
    }
    let clean = context(root).await;
    let line = clean.git.unwrap();
    assert!(line.starts_with("branch main @ "), "{line}");
    assert!(line.ends_with(", clean"), "{line}");
    let head = &line["branch main @ ".len()..line.len() - ", clean".len()];
    assert_eq!(head.len(), 7, "{line}");
    assert!(head.chars().all(|c| c.is_ascii_hexdigit()), "{line}");

    std::fs::write(root.join("src/main.rs"), "fn main() { changed }\n").unwrap();
    std::fs::write(root.join("README.md"), "# changed\n").unwrap();
    std::fs::write(root.join("new.rs"), "\n").unwrap();
    let dirty = context(root).await;
    assert_eq!(
        dirty.git.unwrap(),
        format!("branch main @ {head}, 2 modified, 1 untracked")
    );
}

#[tokio::test]
async fn prompt_show_assembles_for_a_workspace_and_answers_for_a_session() {
    let dir = fixture();
    instructions(dir.path(), "Be brief.\n");
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::from_home(home.path());
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    let loader = ConfigLoader::new(paths);
    let mut config = Config::default();
    config.daemon.secret_store = SecretStoreKind::File;
    let state = AppState::open_with(loader, &config).unwrap();

    // A workspace: assembled now.
    let r = prompt_show(
        &state,
        &PromptShowParams {
            session_id: None,
            workspace: Some(dir.path().to_string_lossy().into_owned()),
            count: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(r.version, PROMPT_VERSION);
    assert_eq!(r.session_id, None);
    assert_eq!(r.blocks.len(), 2);
    assert_eq!(r.blocks[0].text, MENTOR_SYSTEM_V1);
    assert!(r.blocks.iter().all(|b| b.cache));
    assert!(
        r.blocks[1]
            .text
            .ends_with("#instructions (from .harness/HARNESS.md)\nBe brief.\n")
    );
    assert_eq!(r.tokens, None);
    assert_eq!(r.token_error, None);

    // No workspace: the block says so.
    let none = prompt_show(&state, &PromptShowParams::default())
        .await
        .unwrap();
    assert_eq!(none.workspace, None);
    assert!(none.blocks[1].text.starts_with("#workspace\nroot: none"));

    // A session that never ran: assembled for its workspace, and the
    // count is asked for without a key → reported, not fatal.
    let session = state
        .session_create(&SessionCreateParams {
            workspace: Some(dir.path().to_string_lossy().into_owned()),
            title: None,
        })
        .await
        .unwrap()
        .session_id;
    let r = prompt_show(
        &state,
        &PromptShowParams {
            session_id: Some(session.clone()),
            workspace: None,
            count: true,
        },
    )
    .await
    .unwrap();
    assert_eq!(r.session_id, None, "no live conversation yet");
    assert_eq!(
        r.workspace,
        Some(Workspace::open(dir.path()).unwrap().root_string())
    );
    assert!(
        r.blocks[1].text.ends_with(
            "#instructions (from .harness/HARNESS.md)
Be brief.
"
        ),
        "{}",
        r.blocks[1].text
    );
    assert_eq!(r.tokens, None);
    assert!(
        r.token_error.as_deref().unwrap().contains("API key"),
        "{:?}",
        r.token_error
    );

    let missing = prompt_show(
        &state,
        &PromptShowParams {
            session_id: Some("nope".into()),
            workspace: None,
            count: false,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(missing.kind(), Some("not_found"));
    state.close().await;
}
