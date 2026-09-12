//! Workspace tests (task M01-02): the path sandbox, ignore rules, the
//! file index, the registry and its RPC handlers, and migration v002.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use apprentice_api::methods::{WorkspaceAddParams, WorkspaceIdParams};
use apprentice_common::paths::Paths;
use apprentice_core::config::ConfigLoader;
use apprentice_core::trace::{NewSession, SCHEMA_VERSION, TraceStore, WorkspaceId};
use apprentice_core::workspace::{
    FileIndex, PathError, Workspace, WorkspaceError, WorkspaceService, Workspaces,
};
use serde_json::json;
use tempfile::TempDir;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A small tree with every kind of ignore rule in play.
fn sample_tree() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    write(r, "README.md", "# sample");
    write(r, "src/main.rs", "fn main() {}");
    write(r, "src/lib.rs", "");
    write(r, "src/debug.log", "");
    write(r, "sub/keep.txt", "");
    write(r, "sub/secret.txt", "");
    write(r, "sub/.gitignore", "secret.txt\n");
    write(r, "generated/out.rs", "");
    write(r, "img/logo.png", "");
    write(r, ".git/HEAD", "ref: refs/heads/main\n");
    write(r, ".git/refs/heads/main", SHA);
    write(r, ".git/info/exclude", "excluded.txt\n");
    write(r, "excluded.txt", "");
    write(r, "node_modules/x/index.js", "");
    write(r, "target/debug/app", "");
    write(r, ".gitignore", "*.log\n");
    write(r, ".harness/ignore", "generated/\n");
    write(r, ".harness/HARNESS.md", "Be brief.");
    write(r, ".harness/config.toml", "[mentor]\neffort = \"low\"\n");
    write(r, ".github/workflows/ci.yml", "");
    dir
}

fn walked(ws: &Workspace) -> Vec<String> {
    ws.walker()
        .filter_map(Result::ok)
        .filter(|e| e.depth() > 0)
        .map(|e| ws.display(e.path()))
        .collect()
}

// ------------------------------------------------------------------ paths

#[test]
fn resolve_keeps_paths_inside_the_root() {
    let dir = sample_tree();
    let ws = Workspace::open(dir.path()).unwrap();
    let root = ws.root().to_path_buf();
    assert!(root.is_absolute());
    assert_eq!(ws.name(), dir.path().file_name().unwrap().to_str().unwrap());

    // Relative, with either separator; existing or not.
    assert_eq!(
        ws.resolve("src/main.rs").unwrap(),
        root.join("src").join("main.rs")
    );
    assert_eq!(
        ws.resolve("src\\main.rs").unwrap(),
        root.join("src").join("main.rs")
    );
    assert_eq!(
        ws.resolve("./src/../README.md").unwrap(),
        root.join("README.md")
    );
    assert_eq!(
        ws.resolve("new/dir/file.txt").unwrap(),
        root.join("new").join("dir").join("file.txt")
    );
    assert_eq!(ws.resolve("").unwrap(), root);
    assert_eq!(ws.resolve(".").unwrap(), root);

    // Absolute inside the root is fine.
    let abs = root.join("src").join("lib.rs");
    assert_eq!(ws.resolve(&abs.to_string_lossy()).unwrap(), abs);

    // Escapes.
    for bad in ["..", "../x", "src/../../x", "a/b/../../../../etc/passwd"] {
        let err = ws.resolve(bad).unwrap_err();
        assert!(matches!(err, PathError::Outside { .. }), "{bad}: {err}");
        assert!(err.to_string().contains("outside the workspace"), "{err}");
    }
    let outside = dir.path().parent().unwrap().join("elsewhere.txt");
    assert!(matches!(
        ws.resolve(&outside.to_string_lossy()),
        Err(PathError::Outside { .. })
    ));
    assert!(matches!(
        ws.resolve("/etc/passwd"),
        Err(PathError::Outside { .. })
    ));
    assert!(matches!(ws.resolve("a\0b"), Err(PathError::Invalid { .. })));

    // contains / display.
    assert!(ws.contains(&root));
    assert!(ws.contains(&root.join("src").join("main.rs")));
    assert!(ws.contains(&root.join("does").join("not").join("exist")));
    assert!(!ws.contains(dir.path().parent().unwrap()));
    assert!(!ws.contains(Path::new("relative")));
    assert_eq!(ws.display(&root), ".");
    assert_eq!(ws.display(&root.join("src").join("main.rs")), "src/main.rs");
    assert_eq!(
        ws.display(&ws.resolve("sub/keep.txt").unwrap()),
        "sub/keep.txt"
    );
    let shown = ws.display(&outside);
    assert!(
        !shown.contains('\\') && shown.ends_with("/elsewhere.txt"),
        "{shown}"
    );
}

#[cfg(windows)]
#[test]
fn windows_paths_compare_case_insensitively_and_without_verbatim_prefix() {
    let dir = sample_tree();
    let ws = Workspace::open(dir.path()).unwrap();
    let root = ws.root().to_string_lossy().into_owned();
    assert!(!root.starts_with(r"\\?\"), "{root}");
    assert!(root.chars().next().unwrap().is_ascii_uppercase(), "{root}");
    let upper = root.to_uppercase();
    let lower = root.to_lowercase();
    assert!(ws.contains(Path::new(&upper)));
    assert!(ws.contains(&Path::new(&lower).join("SRC").join("MAIN.RS")));
    let resolved = ws.resolve(&format!("{upper}\\src\\main.rs")).unwrap();
    assert!(ws.contains(&resolved));
    assert_eq!(
        ws.display(&Path::new(&lower).join("SRC").join("x.rs")),
        "SRC/x.rs"
    );
    // A verbatim spelling of the root is the root.
    assert_eq!(ws.display(Path::new(&format!(r"\\?\{root}"))), ".");
    // Another drive is outside even when the tail matches.
    let other = if root.starts_with('Z') { "Y" } else { "Z" };
    assert!(matches!(
        ws.resolve(&format!("{other}{}", &root[1..])),
        Err(PathError::Outside { .. })
    ));
}

#[test]
#[cfg_attr(
    windows,
    ignore = "creating symlinks needs developer mode or elevation on Windows"
)]
fn symlinks_that_leave_the_root_resolve_outside() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir_all(root.join("inner")).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    write(&elsewhere, "secret.txt", "s");
    write(&root, "inner/ok.txt", "ok");
    symlink_dir(&elsewhere, &root.join("escape")).unwrap();
    symlink_file(&elsewhere.join("secret.txt"), &root.join("alias.txt")).unwrap();
    symlink_dir(&root.join("inner"), &root.join("inner_link")).unwrap();

    let ws = Workspace::open(&root).unwrap();
    assert!(matches!(
        ws.resolve("escape/secret.txt"),
        Err(PathError::Outside { .. })
    ));
    assert!(matches!(
        ws.resolve("escape/new.txt"),
        Err(PathError::Outside { .. })
    ));
    assert!(matches!(
        ws.resolve("alias.txt"),
        Err(PathError::Outside { .. })
    ));
    assert!(!ws.contains(&root.join("escape").join("secret.txt")));
    // A symlink that stays inside resolves to its target.
    assert_eq!(
        ws.resolve("inner_link/ok.txt").unwrap(),
        ws.root().join("inner").join("ok.txt")
    );
    // Symlinks are not followed by discovery.
    let files = walked(&ws);
    assert!(files.contains(&"inner/ok.txt".to_owned()), "{files:?}");
    assert!(!files.iter().any(|f| f.starts_with("escape/")), "{files:?}");

    // A symlinked root opens as its target.
    let link_root = dir.path().join("root_link");
    symlink_dir(&root, &link_root).unwrap();
    let via_link = Workspace::open(&link_root).unwrap();
    assert_eq!(via_link.root(), ws.root());
}

#[cfg(unix)]
fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(unix)]
fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[cfg(windows)]
fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

/// Junctions need no privilege on Windows and canonicalise the same way
/// symlinked directories do, so the escape check gets exercised here.
#[cfg(windows)]
#[test]
fn junctions_that_leave_the_root_resolve_outside() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir_all(root.join("inner")).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    write(&elsewhere, "secret.txt", "s");
    write(&root, "inner/ok.txt", "ok");
    let junction = |link: &Path, target: &Path| {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "mklink /J failed");
    };
    junction(&root.join("escape"), &elsewhere);
    junction(&root.join("inner_link"), &root.join("inner"));

    let ws = Workspace::open(&root).unwrap();
    assert!(matches!(
        ws.resolve("escape/secret.txt"),
        Err(PathError::Outside { .. })
    ));
    assert!(matches!(
        ws.resolve("escape/new.txt"),
        Err(PathError::Outside { .. })
    ));
    assert!(matches!(
        ws.resolve("escape"),
        Err(PathError::Outside { .. })
    ));
    assert!(!ws.contains(&root.join("escape").join("secret.txt")));
    assert_eq!(
        ws.resolve("inner_link/ok.txt").unwrap(),
        ws.root().join("inner").join("ok.txt")
    );
    let files = walked(&ws);
    assert!(files.contains(&"inner/ok.txt".to_owned()), "{files:?}");
    assert!(!files.iter().any(|f| f.starts_with("escape/")), "{files:?}");

    let link_root = dir.path().join("root_link");
    junction(&link_root, &root);
    let via_link = Workspace::open(&link_root).unwrap();
    assert_eq!(via_link.root(), ws.root());
}

#[test]
fn opening_a_missing_or_file_root_fails() {
    let dir = tempfile::tempdir().unwrap();
    let err = Workspace::open(&dir.path().join("nope")).unwrap_err();
    assert!(matches!(err, WorkspaceError::Root { .. }), "{err}");
    assert!(err.to_string().contains("workspace root"), "{err}");
    write(dir.path(), "file.txt", "");
    let err = Workspace::open(&dir.path().join("file.txt")).unwrap_err();
    assert!(matches!(err, WorkspaceError::Root { .. }), "{err}");
}

// ------------------------------------------------------------- discovery

#[test]
fn walker_honours_gitignore_chain_harness_ignore_and_builtins() {
    let dir = sample_tree();
    let ws = Workspace::open(dir.path()).unwrap();
    let files = walked(&ws);
    assert_eq!(
        files,
        [
            ".github",
            ".github/workflows",
            ".github/workflows/ci.yml",
            ".gitignore",
            ".harness",
            ".harness/HARNESS.md",
            "README.md",
            "img",
            "src",
            "src/lib.rs",
            "src/main.rs",
            "sub",
            "sub/.gitignore",
            "sub/keep.txt",
        ]
    );
    assert!(ws.is_ignored(Path::new("generated/out.rs"), false));
    assert!(ws.is_ignored(Path::new(".harness/config.toml"), false));
    assert!(!ws.is_ignored(Path::new(".harness/HARNESS.md"), false));
    assert!(ws.has_instructions());
    assert_eq!(ws.instructions().unwrap().as_deref(), Some("Be brief."));
    assert!(ws.config_file().is_file());
    assert!(ws.ignore_file().is_file());
    assert!(!ws.permissions_file().exists());
    assert_eq!(ws.harness_dir(), ws.root().join(".harness"));
}

#[test]
fn index_is_cached_and_rebuilt_on_refresh() {
    let dir = sample_tree();
    let ws = Workspace::open(dir.path()).unwrap();
    assert!(ws.cached_index().is_none());
    let a = ws.index();
    let paths: Vec<&str> = a.entries().iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            ".github/workflows/ci.yml",
            ".gitignore",
            ".harness/HARNESS.md",
            "README.md",
            "src/lib.rs",
            "src/main.rs",
            "sub/.gitignore",
            "sub/keep.txt",
        ]
    );
    assert_eq!(a.get("src/main.rs").unwrap().language, Some("rust"));
    assert_eq!(a.get("src/main.rs").unwrap().size, 12);
    assert_eq!(a.directories(), 6);
    assert!(!a.is_truncated());
    let b = ws.index();
    assert!(Arc::ptr_eq(&a, &b), "fresh index is reused");
    assert!(Arc::ptr_eq(&a, &ws.cached_index().unwrap()));

    // New files and new ignore rules show up after a refresh.
    write(dir.path(), "src/new.rs", "");
    write(dir.path(), ".harness/ignore", "generated/\nsub/\n");
    assert!(Arc::ptr_eq(&a, &ws.index()));
    let c = ws.refresh();
    assert!(!Arc::ptr_eq(&a, &c));
    let paths: Vec<&str> = c.entries().iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            ".github/workflows/ci.yml",
            ".gitignore",
            ".harness/HARNESS.md",
            "README.md",
            "src/lib.rs",
            "src/main.rs",
            "src/new.rs",
        ]
    );
    assert!(Arc::ptr_eq(&c, &ws.index()));
}

/// Acceptance: a 50k-file tree indexes in < 2 s on the reference
/// machine. Creating the tree takes far longer than indexing it, hence
/// ignored by default.
#[test]
#[ignore = "timing-sensitive; run with --ignored on the reference machine"]
fn fifty_thousand_file_tree_indexes_in_under_two_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for d in 0..500 {
        let sub = root.join(format!("pkg{d:03}")).join("src");
        std::fs::create_dir_all(&sub).unwrap();
        for f in 0..100 {
            std::fs::write(sub.join(format!("file{f:03}.rs")), "// x\n").unwrap();
        }
    }
    let ws = Workspace::open(root).unwrap();
    let index = ws.index();
    eprintln!(
        "50k files indexed in {:?} ({} files, {} dirs)",
        index.build_time(),
        index.len(),
        index.directories()
    );
    assert_eq!(index.len(), 50_000);
    assert!(
        index.build_time() < std::time::Duration::from_secs(2),
        "{:?}",
        index.build_time()
    );
    let again = FileIndex::build(ws.root(), &ws.ignore_rules());
    assert!(again.build_time() < std::time::Duration::from_secs(2));
}

// -------------------------------------------------------------- registry

struct Home {
    _dir: TempDir,
    paths: Paths,
    store: Arc<TraceStore>,
}

fn home() -> Home {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::from_home(dir.path().join("home"));
    let store = Arc::new(TraceStore::open(&paths).unwrap());
    Home {
        _dir: dir,
        paths,
        store,
    }
}

fn service(h: &Home) -> (Arc<Workspaces>, WorkspaceService) {
    let workspaces = Arc::new(Workspaces::new(Arc::clone(&h.store)));
    let svc = WorkspaceService::new(Arc::clone(&workspaces), ConfigLoader::new(h.paths.clone()));
    (workspaces, svc)
}

#[tokio::test]
async fn add_is_idempotent_and_info_reports_the_tree() {
    let h = home();
    let tree = sample_tree();
    let (workspaces, svc) = service(&h);

    let a = svc
        .add(&WorkspaceAddParams {
            root: tree.path().to_string_lossy().into_owned(),
            name: Some("Sample".into()),
        })
        .unwrap();
    // Same root (spelled differently) → same id; the name sticks.
    let spelled = tree.path().join("src").join("..");
    let b = svc
        .add(&WorkspaceAddParams {
            root: spelled.to_string_lossy().into_owned(),
            name: Some("Other".into()),
        })
        .unwrap();
    assert_eq!(a.id, b.id);
    assert_eq!(b.name, "Sample");
    assert!(b.last_used_at >= a.last_used_at);
    assert!(Path::new(&a.root).is_absolute());
    assert_eq!(svc.list().unwrap().workspaces.len(), 1);
    assert_eq!(workspaces.open_count(), 1);

    let err = svc
        .add(&WorkspaceAddParams {
            root: "relative/path".into(),
            name: None,
        })
        .unwrap_err();
    assert!(err.to_string().contains("must be absolute"), "{err}");
    let err = svc
        .add(&WorkspaceAddParams {
            root: tree.path().join("missing").to_string_lossy().into_owned(),
            name: None,
        })
        .unwrap_err();
    assert!(err.to_string().contains("workspace root"), "{err}");

    let info = svc
        .info(&WorkspaceIdParams { id: a.id.clone() }, false)
        .await
        .unwrap();
    assert_eq!(info.id, a.id);
    assert_eq!(info.name, "Sample");
    assert_eq!(info.file_count, 8);
    assert!(!info.index_truncated);
    assert_eq!(info.git_head.as_deref(), Some(SHA));
    assert_eq!(info.git_branch.as_deref(), Some("main"));
    assert!(info.has_instructions);
    assert!(info.has_config);
    assert!(info.has_ignore_file);
    assert_eq!(info.config_overrides, ["mentor.effort"]);

    // The same handle serves every caller; refresh picks up changes.
    write(tree.path(), "added.txt", "");
    let same = svc
        .info(&WorkspaceIdParams { id: a.id.clone() }, false)
        .await
        .unwrap();
    assert_eq!(same.file_count, 8);
    let refreshed = svc
        .info(&WorkspaceIdParams { id: a.id.clone() }, true)
        .await
        .unwrap();
    assert_eq!(refreshed.file_count, 9);
    assert_eq!(refreshed.index_age_s, 0);
    let ws = workspaces.get(&WorkspaceId::from(a.id.as_str())).unwrap();
    assert_eq!(ws.index().len(), 9);
    assert_eq!(ws.id().map(ToString::to_string), Some(a.id.clone()));

    // A root without a repo or .harness reports none of that.
    let plain = tempfile::tempdir().unwrap();
    let p = svc
        .add(&WorkspaceAddParams {
            root: plain.path().to_string_lossy().into_owned(),
            name: None,
        })
        .unwrap();
    assert_eq!(p.name, plain.path().file_name().unwrap().to_str().unwrap());
    let info = svc
        .info(&WorkspaceIdParams { id: p.id.clone() }, false)
        .await
        .unwrap();
    assert_eq!(info.file_count, 0);
    assert_eq!(info.git_head, None);
    assert_eq!(info.git_branch, None);
    assert!(!info.has_instructions && !info.has_config && !info.has_ignore_file);
    assert!(info.config_overrides.is_empty());
    // Most recently used first.
    let ids: Vec<String> = svc
        .list()
        .unwrap()
        .workspaces
        .into_iter()
        .map(|w| w.id)
        .collect();
    assert_eq!(ids, [p.id.clone(), a.id.clone()]);

    let err = svc
        .info(&WorkspaceIdParams { id: "nope".into() }, false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
}

#[test]
fn remove_unlinks_sessions_and_keeps_their_path() {
    let h = home();
    let tree = sample_tree();
    let (workspaces, svc) = service(&h);
    let ws = workspaces.add(tree.path(), None).unwrap();
    let id = ws.id().unwrap().clone();
    let sid = h
        .store
        .create_session(&NewSession {
            title: None,
            workspace_path: Some(ws.root_string()),
            workspace_id: Some(id.clone()),
            config: json!({}),
        })
        .unwrap();
    assert_eq!(
        h.store.get_session(&sid).unwrap().workspace_id.as_ref(),
        Some(&id)
    );
    // Opening by root finds the registered handle.
    let by_root = workspaces.open_root(tree.path()).unwrap();
    assert!(Arc::ptr_eq(&ws, &by_root));

    let r = svc
        .remove(&WorkspaceIdParams { id: id.to_string() })
        .unwrap();
    assert_eq!(r.sessions_unlinked, 1);
    let session = h.store.get_session(&sid).unwrap();
    assert_eq!(session.workspace_id, None);
    assert_eq!(
        session.workspace_path.as_deref(),
        Some(ws.root_string().as_str())
    );
    assert!(svc.list().unwrap().workspaces.is_empty());
    assert_eq!(workspaces.open_count(), 0);
    assert!(matches!(workspaces.get(&id), Err(WorkspaceError::Trace(_))));
    let err = svc
        .remove(&WorkspaceIdParams { id: id.to_string() })
        .unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
    // An unregistered root still opens ad hoc.
    let adhoc = workspaces.open_root(tree.path()).unwrap();
    assert_eq!(adhoc.id(), None);
    assert_eq!(workspaces.open_count(), 0);

    // A registered root that disappears is reported, not crashed on.
    let gone = tempfile::tempdir().unwrap();
    let gone_id = workspaces
        .add(gone.path(), None)
        .unwrap()
        .id()
        .unwrap()
        .clone();
    drop(gone);
    let fresh = Workspaces::new(Arc::clone(&h.store));
    let err = fresh.get(&gone_id).unwrap_err();
    assert!(matches!(err, WorkspaceError::Root { .. }), "{err}");
}

// ------------------------------------------------------------- migration

/// An M00 database (schema v1 with sessions, events and blobs) opens
/// with this build: v002 applies, rows survive, and the new column and
/// table are usable.
#[test]
fn migration_v002_applies_to_an_m00_database() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::from_home(dir.path());
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    let db = paths.data_dir.join("traces.sqlite");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(include_str!("../src/trace/migrations/v001.sql"))
            .unwrap();
        conn.execute_batch(
            "INSERT INTO schema_meta(key, value) VALUES ('version', '1');
             INSERT INTO sessions(id, created_at, updated_at, title, workspace_path, config_json)
             VALUES ('s1', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', 'old', 'C:/old/repo', '{}');
             INSERT INTO events(id, session_id, seq, ts, kind, payload_json)
             VALUES ('e1', 's1', 1, '2026-01-01T00:00:00.000Z', 'session.created', '{}');
             INSERT INTO blobs(id, size, media_type, created_at) VALUES ('b1', 3, 'text/plain', 't0');",
        )
        .unwrap();
    }

    let store = TraceStore::open(&paths).unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 2);
    let session = store.get_session(&"s1".into()).unwrap();
    assert_eq!(session.title.as_deref(), Some("old"));
    assert_eq!(session.workspace_path.as_deref(), Some("C:/old/repo"));
    assert_eq!(session.workspace_id, None);
    assert_eq!(store.event_count(&"s1".into()).unwrap(), 1);
    assert!(store.list_workspaces().unwrap().is_empty());
    let usage = store.disk_usage().unwrap();
    assert_eq!((usage.sessions, usage.events, usage.blob_rows), (1, 1, 1));

    // The new table works on the migrated file.
    let tree = tempfile::tempdir().unwrap();
    let workspaces = Workspaces::new(Arc::new(store));
    let ws = workspaces.add(tree.path(), Some("t")).unwrap();
    assert_eq!(ws.name(), "t");
    assert_eq!(workspaces.list().unwrap().len(), 1);
    let root: PathBuf = workspaces.list().unwrap()[0].root.clone().into();
    assert_eq!(root, ws.root());
}
