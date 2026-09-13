//! Trace bundles (task M01-14): export → import → export again is the
//! same bundle; redaction replaces the secrets of a session everywhere
//! and consistently; `replay-check` passes (with `--rebuild`) for a real
//! trajectory and fails on a tampered body; a corrupted bundle blob is
//! refused by name; the RPC methods answer over a router.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apprentice_api::events::Event;
use apprentice_api::jsonrpc::codes;
use apprentice_api::methods::{
    SessionCreateParams, TraceExport, TraceExportParams, TraceImport, TraceImportParams,
    TraceReplayCheck, TraceReplayCheckParams,
};
use apprentice_api::server::{Router, RouterConfig};
use apprentice_api::types::{BundleSelection, PermissionMode, ReplayStatus, RunOptions};
use apprentice_client::{ClientError, ClientOptions, DaemonClient};
use apprentice_core::app::AppState;
use apprentice_core::bundle::{
    BundleError, BundleService, ExportOptions, ImportOptions, RedactOptions, ReplaySelection,
    export, format, import, replay_check,
};
use apprentice_core::config::{ConfigLoader, Paths};
use apprentice_core::mentor::{MentorRequest, Message, request_body};
use apprentice_core::runtime::run_agent;
use apprentice_core::secrets::{Secret, SecretStore as _, api_key_name};
use apprentice_core::trace::{
    BlobId, CallFilter, CallId, CallKind, NewAgent, NewSession, SessionId, StepRef, TraceStore,
    kinds,
};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const KEY: &str = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789";
const PEM: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkq\n-----END PRIVATE KEY-----";
const TICKET: &str = "ACME-4242";

fn fixture(name: &str) -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sse")
        .join(format!("{name}.txt"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn sse(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(fixture(name), "text/event-stream")
}

struct Script(Mutex<VecDeque<ResponseTemplate>>);

impl Respond for Script {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        self.0.lock().unwrap().pop_front().unwrap_or_else(|| {
            ResponseTemplate::new(500).set_body_json(json!({
                "type": "error",
                "error": {"type": "api_error", "message": "script exhausted"}
            }))
        })
    }
}

/// A state whose mentor plays the three-step trajectory of the loop
/// tests, on a workspace with the two files it reads.
struct Harness {
    home: tempfile::TempDir,
    workspace: tempfile::TempDir,
    state: Arc<AppState>,
    server: MockServer,
}

impl Harness {
    async fn new() -> Self {
        let server = MockServer::start().await;
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(home.path());
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        std::fs::write(
            paths.config_file(),
            format!(
                "[daemon]\nsecret_store = \"file\"\n\n[sessions]\nauto_title = false\n\n[mentor]\nbase_url = \"{}\"\nmax_retries = 0\ntimeout_s = 30\n",
                server.uri()
            ),
        )
        .unwrap();
        let loader = ConfigLoader::new(paths);
        let config = loader.load(None).unwrap().config;
        let state = AppState::open_with(loader, &config).unwrap();
        state
            .secrets()
            .set(&api_key_name("anthropic"), &Secret::new("sk-ant-test"))
            .unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("src")).unwrap();
        std::fs::write(
            workspace.path().join("src/main.rs"),
            "fn main() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        std::fs::write(
            workspace.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(Script(Mutex::new(
                vec![sse("tool_use_parallel"), sse("tool_use_write"), sse("text")].into(),
            )))
            .mount(&server)
            .await;
        Self {
            home,
            workspace,
            state,
            server,
        }
    }

    fn store(&self) -> &Arc<TraceStore> {
        self.state.store()
    }

    fn paths(&self) -> Paths {
        Paths::from_home(self.home.path())
    }

    /// Runs the trajectory on a new session with `prompt`.
    async fn trajectory(&self, prompt: &str) -> SessionId {
        let session: SessionId = self
            .state
            .session_create(&SessionCreateParams {
                workspace: Some(self.workspace.path().to_string_lossy().into_owned()),
                title: Some("bundle test".into()),
            })
            .await
            .unwrap()
            .session_id
            .into();
        let (_handle, mut rx) = run_agent(
            &self.state,
            session.clone(),
            prompt.into(),
            RunOptions {
                permission_mode: Some(PermissionMode::Auto),
                ..RunOptions::default()
            },
        )
        .await
        .unwrap();
        collect(&mut rx).await;
        assert_eq!(self.server.received_requests().await.unwrap().len(), 3);
        session
    }
}

async fn collect(rx: &mut broadcast::Receiver<Event>) -> Vec<Event> {
    let mut out = Vec::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(15), rx.recv())
            .await
            .expect("agent finished in time")
            .expect("channel open");
        let done = ev.is_terminal();
        out.push(ev);
        if done {
            return out;
        }
    }
}

/// A fresh, empty store in `dir`.
fn fresh_store(dir: &Path) -> Arc<TraceStore> {
    Arc::new(TraceStore::open_at(dir.join("traces.sqlite"), dir.join("blobs"), 1024).unwrap())
}

fn select(ids: &[&SessionId]) -> BundleSelection {
    BundleSelection {
        session_ids: ids.iter().map(ToString::to_string).collect(),
        ..BundleSelection::default()
    }
}

fn opts(output: PathBuf, sel: BundleSelection, redact: Option<RedactOptions>) -> ExportOptions {
    ExportOptions {
        output,
        selection: sel,
        redact,
    }
}

/// Every file under `dir`, as bytes.
fn files_of(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push((p.clone(), std::fs::read(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// The rows of a bundle with the import provenance and the manifest's
/// timestamp taken out, so two exports of the same data compare equal.
fn comparable(dir: &Path) -> Vec<(String, String)> {
    files_of(dir)
        .into_iter()
        .map(|(p, bytes)| {
            let name = p
                .strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let mut text = String::from_utf8_lossy(&bytes).into_owned();
            if name == "sessions.jsonl" {
                text = text
                    .lines()
                    .map(|l| {
                        let mut v: Value = serde_json::from_str(l).unwrap();
                        v["config"].as_object_mut().unwrap().remove("import");
                        v.to_string()
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
            } else if name == "manifest.json" {
                let mut v: Value = serde_json::from_str(&text).unwrap();
                v["created_at"] = Value::Null;
                text = v.to_string();
            }
            (name, text)
        })
        .collect()
}

#[tokio::test]
async fn replay_check_passes_for_a_real_trajectory_and_catches_a_tampered_body() {
    let h = Harness::new().await;
    let session = h.trajectory("add hello").await;
    let store = h.store();

    let report = replay_check(store, &ReplaySelection::default(), true).unwrap();
    assert_eq!((report.checked, report.passed, report.failed), (3, 3, 0));
    for c in &report.calls {
        assert_eq!(c.status, ReplayStatus::Ok, "{c:?}");
        assert_eq!(
            c.checks,
            ["blob", "hash", "parse", "canonical", "rebuild"],
            "{c:?}"
        );
        assert_eq!(c.session_id, session.to_string());
    }
    // One call by id, without the rebuild.
    let one = replay_check(
        store,
        &ReplaySelection {
            call_id: Some(report.calls[1].call_id.clone()),
            ..ReplaySelection::default()
        },
        false,
    )
    .unwrap();
    assert_eq!(one.calls.len(), 1);
    assert_eq!(one.calls[0].checks.len(), 4);
    assert!(!one.rebuild);
    assert!(matches!(
        replay_check(
            store,
            &ReplaySelection {
                call_id: Some("nope".into()),
                ..ReplaySelection::default()
            },
            false
        ),
        Err(BundleError::Trace(
            apprentice_core::trace::TraceError::NotFound { .. }
        ))
    ));

    // A title call is not rebuilt from the conversation: skipped, not failed.
    let agent = store
        .start_agent(&NewAgent::main(session.clone(), "title"))
        .unwrap();
    let step = store.start_step(&agent).unwrap();
    let req = MentorRequest {
        model: "claude-haiku-4-5".into(),
        max_tokens: 40,
        system: vec![],
        messages: vec![Message::user("title this")],
        tools: vec![],
        thinking: apprentice_core::mentor::Thinking::Disabled,
        effort: apprentice_api::types::Effort::Low,
        metadata: None,
    };
    let body = request_body(&req).unwrap();
    let title_call = CallId::generate();
    store
        .record_mentor_request(
            &StepRef {
                session: session.clone(),
                agent,
                step,
            },
            &title_call,
            CallKind::Title,
            &req,
            &body,
            None,
        )
        .unwrap();
    let report = replay_check(store, &ReplaySelection::default(), true).unwrap();
    assert_eq!(
        (report.checked, report.passed, report.failed, report.skipped),
        (4, 3, 0, 1)
    );
    let skipped = report
        .calls
        .iter()
        .find(|c| c.status == ReplayStatus::Skipped)
        .unwrap();
    assert_eq!(skipped.kind, "title");
    assert!(skipped.problems[0].contains("rebuild skipped"));

    // Tamper with a stored body: the hash check names the call.
    let calls = store
        .list_mentor_calls(&CallFilter::session_of(&session))
        .unwrap();
    let ev = store.get_event(&calls[0].request_event_id).unwrap();
    let blob = BlobId::from(ev.blob_id.as_deref().unwrap());
    let path = store.blob_path(&blob);
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"{\"model\":\"tampered\"}").unwrap();
    let report = replay_check(store, &ReplaySelection::default(), false).unwrap();
    assert_eq!(report.failed, 1);
    let bad = report
        .calls
        .iter()
        .find(|c| c.status == ReplayStatus::Failed)
        .unwrap();
    assert_eq!(bad.call_id, calls[0].id.to_string());
    assert_eq!(bad.checks, ["blob", "hash"]);
    assert!(bad.problems[0].contains("hashes to"), "{:?}", bad.problems);
    std::fs::write(&path, original).unwrap();

    // A rebuild sees a drifted conversation: replace a stored message.
    let rows = store.session_messages(&session, 0, None).unwrap();
    assert!(rows.len() >= 2);
    store
        .put_session_message(&apprentice_core::trace::NewMessage {
            session: session.clone(),
            seq: 1,
            role: rows[0].role,
            content: vec![apprentice_core::mentor::ContentBlock::text(
                "something else",
            )],
            agent: rows[0].agent_id.clone(),
            step: rows[0].step_id.clone(),
        })
        .unwrap();
    let report = replay_check(store, &ReplaySelection::default(), true).unwrap();
    assert_eq!(report.failed, 3, "{report:#?}");
    let first = &report.calls[0];
    assert_eq!(first.checks.last().unwrap(), "rebuild");
    assert!(
        first.problems[0].contains("$.messages[0].content[0].text"),
        "{:?}",
        first.problems
    );
}

#[tokio::test]
async fn export_import_export_is_the_same_bundle() {
    let h = Harness::new().await;
    let session = h.trajectory("add hello").await;
    let store = h.store();
    let out = tempfile::tempdir().unwrap();

    let first = out.path().join("first");
    let (path, manifest) = export(store, &opts(first.clone(), select(&[&session]), None)).unwrap();
    assert_eq!(path, first);
    assert_eq!(manifest.format_version, 1);
    assert_eq!(manifest.counts.sessions, 1);
    assert_eq!(manifest.counts.workspaces, 1);
    assert_eq!(manifest.counts.mentor_calls, 3);
    assert_eq!(manifest.counts.agents, 1);
    assert_eq!(manifest.counts.steps, 3);
    assert!(manifest.counts.messages >= 6, "{:?}", manifest.counts);
    assert!(manifest.counts.blobs >= 6, "{:?}", manifest.counts);
    assert!(manifest.redaction.is_none());
    assert_eq!(manifest.sessions[0].id, session.to_string());
    assert_eq!(manifest.sessions[0].mentor_calls, 3);
    assert_eq!(manifest.selection.session_ids, [session.to_string()]);
    // Every blob file hashes to its name and is in blobs.jsonl.
    let rows = format::Rows::read(&first).unwrap();
    for b in &rows.blobs {
        let bytes = format::read_blob(&first, &b.id).unwrap();
        assert_eq!(apprentice_core::trace::sha256_hex(&bytes), b.id);
        assert_eq!(bytes.len() as u64, b.size);
    }
    assert_eq!(
        rows.events
            .iter()
            .filter(|e| e.kind == kinds::MENTOR_REQUEST)
            .count(),
        3
    );

    // Into a fresh store, keeping ids, then out again.
    let other_dir = tempfile::tempdir().unwrap();
    let other = fresh_store(other_dir.path());
    let report = import(
        &other,
        &ImportOptions {
            path: first.clone(),
            into_workspace: None,
            keep_ids: true,
        },
    )
    .unwrap();
    assert_eq!(report.sessions.len(), 1);
    assert_eq!(report.sessions[0].from, report.sessions[0].to);
    assert_eq!(report.counts, manifest.counts);
    assert!(!report.redacted);
    assert_eq!(report.blobs_written, manifest.counts.blobs);
    let copied = other.get_session(&session).unwrap();
    assert_eq!(copied.title.as_deref(), Some("bundle test"));
    let original = store.get_session(&session).unwrap();
    assert_eq!(
        copied.workspace_id, original.workspace_id,
        "the root exists on this machine: the workspace is registered again"
    );
    assert_eq!(copied.workspace_path, original.workspace_path);
    assert_eq!(
        other.list_workspaces().unwrap()[0].root,
        store.list_workspaces().unwrap()[0].root
    );
    assert_eq!(
        copied.config["import"]["original_session_id"],
        session.to_string()
    );
    assert_eq!(copied.config["import"]["redacted"], false);
    assert_eq!(
        other.session_messages(&session, 0, None).unwrap().len() as u64,
        manifest.counts.messages
    );
    assert!(other.integrity_check().unwrap().is_ok());
    let hits = other.search_sessions("hello", false, None).unwrap();
    assert!(!hits.is_empty(), "the search index is rebuilt");

    let second = out.path().join("second");
    let (_, manifest2) = export(&other, &opts(second.clone(), select(&[&session]), None)).unwrap();
    assert_eq!(manifest2.counts, manifest.counts);
    assert_eq!(comparable(&first), comparable(&second));

    // The copy replays, rebuild included.
    let replay = replay_check(&other, &ReplaySelection::default(), true).unwrap();
    assert_eq!((replay.checked, replay.passed), (3, 3), "{replay:#?}");

    // The same bundle again: fresh ids, every reference remapped.
    let report = import(
        &other,
        &ImportOptions {
            path: first.clone(),
            into_workspace: None,
            keep_ids: false,
        },
    )
    .unwrap();
    let copy = SessionId::from(report.sessions[0].to.as_str());
    assert_ne!(copy, session);
    assert_eq!(report.blobs_written, 0, "content-addressed: nothing new");
    let calls = other
        .list_mentor_calls(&CallFilter::session_of(&copy))
        .unwrap();
    assert_eq!(calls.len(), 3);
    for c in &calls {
        let ev = other.get_event(&c.request_event_id).unwrap();
        assert_eq!(ev.summary.session_id, copy.to_string());
        assert_eq!(
            ev.payload["call_id"],
            c.id.to_string(),
            "payload ids follow"
        );
        assert_eq!(ev.summary.agent_id.as_deref(), Some(c.agent_id.as_str()));
    }
    let replay = replay_check(&other, &ReplaySelection::default(), true).unwrap();
    assert_eq!((replay.checked, replay.passed), (6, 6), "{replay:#?}");
    assert!(other.integrity_check().unwrap().is_ok());
    let err = import(
        &other,
        &ImportOptions {
            path: first.clone(),
            into_workspace: None,
            keep_ids: true,
        },
    )
    .unwrap_err();
    assert!(matches!(err, BundleError::Conflict(_)), "{err}");

    // Attached to a workspace on import.
    let ws = other.add_workspace("C:/src/elsewhere", None).unwrap();
    let report = import(
        &other,
        &ImportOptions {
            path: first.clone(),
            into_workspace: Some(ws.id.to_string()),
            keep_ids: false,
        },
    )
    .unwrap();
    let attached = other
        .get_session(&SessionId::from(report.sessions[0].to.as_str()))
        .unwrap();
    assert_eq!(attached.workspace_id, Some(ws.id.clone()));
    assert!(matches!(
        import(
            &other,
            &ImportOptions {
                path: first.clone(),
                into_workspace: Some("nope".into()),
                keep_ids: false,
            },
        ),
        Err(BundleError::NotFound {
            what: "workspace",
            ..
        })
    ));

    // Packed: the same rows, through a `.tar.zst`.
    let packed = out.path().join("traces.tar.zst");
    let (path, manifest3) =
        export(store, &opts(packed.clone(), select(&[&session]), None)).unwrap();
    assert_eq!(path, packed);
    assert!(packed.is_file());
    assert_eq!(manifest3.counts, manifest.counts);
    assert!(
        std::fs::read_dir(out.path()).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")),
        "no temp dir left behind"
    );
    let third_dir = tempfile::tempdir().unwrap();
    let third = fresh_store(third_dir.path());
    let report = import(
        &third,
        &ImportOptions {
            path: packed.clone(),
            into_workspace: None,
            keep_ids: true,
        },
    )
    .unwrap();
    assert_eq!(report.counts, manifest.counts);
    let fourth = out.path().join("fourth");
    export(&third, &opts(fourth.clone(), select(&[&session]), None)).unwrap();
    assert_eq!(comparable(&first), comparable(&fourth));

    // Selection errors and an output that is in the way.
    let err = export(
        store,
        &opts(out.path().join("x"), BundleSelection::default(), None),
    )
    .unwrap_err();
    assert!(matches!(err, BundleError::Invalid(_)), "{err}");
    let err = export(
        store,
        &opts(
            out.path().join("x"),
            select(&[&SessionId::from("nope")]),
            None,
        ),
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            BundleError::NotFound {
                what: "session",
                ..
            }
        ),
        "{err}"
    );
    let err = export(store, &opts(first.clone(), select(&[&session]), None)).unwrap_err();
    assert!(matches!(err, BundleError::Invalid(_)), "{err}");
    let all = export(
        store,
        &opts(
            out.path().join("all"),
            BundleSelection {
                all: true,
                ..BundleSelection::default()
            },
            None,
        ),
    )
    .unwrap();
    assert_eq!(all.1.counts.sessions, 1);
    let none = export(
        store,
        &opts(
            out.path().join("none"),
            BundleSelection {
                since: Some("2999-01-01T00:00:00.000Z".into()),
                ..BundleSelection::default()
            },
            None,
        ),
    )
    .unwrap_err();
    assert!(matches!(none, BundleError::Invalid(_)), "{none}");
}

#[tokio::test]
async fn redaction_replaces_secrets_everywhere_and_the_copy_still_replays() {
    let h = Harness::new().await;
    std::fs::write(
        h.paths().config_dir.join("redact.toml"),
        "[[pattern]]\nname = \"ticket\"\nregex = \"ACME-[0-9]+\"\n",
    )
    .unwrap();
    let prompt = format!("add hello; the key is {KEY}, cert:\n{PEM}\nsee {TICKET} and {KEY}");
    let session = h.trajectory(&prompt).await;
    let store = h.store();
    let out = tempfile::tempdir().unwrap();

    let redact = RedactOptions {
        builtins: true,
        user: apprentice_core::bundle::RedactConfig::load(&h.paths().config_dir).unwrap(),
        workspace_patterns: true,
        paths: true,
        home: Some(h.home.path().to_string_lossy().into_owned()),
    };
    let dir = out.path().join("redacted");
    let (_, manifest) =
        export(store, &opts(dir.clone(), select(&[&session]), Some(redact))).unwrap();
    let r = manifest.redaction.as_ref().unwrap();
    assert!(r.applied);
    assert!(r.paths);
    assert!(!r.replayable);
    assert_eq!(r.touched_requests, 3);
    let matches = |name: &str| r.rules.iter().find(|x| x.name == name).unwrap().matches;
    assert!(matches("anthropic_key") >= 2, "{r:#?}");
    assert!(matches("pem") >= 1, "{r:#?}");
    assert!(matches("ticket") >= 1, "{r:#?}");
    assert!(
        matches("paths") >= 1,
        "the workspace root is in the system prompt: {r:#?}"
    );
    assert_eq!(
        r.rules.iter().find(|x| x.name == "ticket").unwrap().source,
        "user"
    );
    assert_eq!(
        r.secrets, 3,
        "the key, the PEM block and the ticket: {r:#?}"
    );
    assert_eq!(
        r.blob_map.len() as u64,
        r.touched_requests + 1,
        "3 bodies and the prompt blob"
    );
    assert!(r.replacements >= matches("anthropic_key") + matches("pem") + matches("ticket"));

    // The originals are in no file of the bundle; the tokens are.
    let root = h.workspace.path().to_string_lossy().replace('\\', "/");
    let mut saw_token = false;
    for (p, bytes) in files_of(&dir) {
        let text = String::from_utf8_lossy(&bytes);
        for secret in [KEY, PEM, TICKET, root.as_str()] {
            assert!(
                !text.contains(secret),
                "{} still holds {}",
                p.display(),
                &secret[..12]
            );
        }
        saw_token |= text.contains("<REDACTED:anthropic_key:1>");
    }
    assert!(saw_token);
    let rows = format::Rows::read(&dir).unwrap();
    let task = rows.agents[0].task_text.as_deref().unwrap();
    assert!(task.contains("<REDACTED:anthropic_key:1>") && task.contains("<REDACTED:pem:"));
    assert!(task.contains("<REDACTED:ticket:"));
    assert_eq!(rows.sessions[0].workspace_path.as_deref(), Some("<WS>"));
    assert_eq!(rows.workspaces[0].root, "<WS>");
    let request = rows
        .events
        .iter()
        .find(|e| e.kind == kinds::MENTOR_REQUEST)
        .unwrap();
    assert_eq!(request.payload["redacted"], true);
    assert_eq!(
        request.payload["request_hash"],
        request.blob_id.clone().unwrap()
    );
    for b in &rows.blobs {
        let bytes = format::read_blob(&dir, &b.id).unwrap();
        assert_eq!(
            apprentice_core::trace::sha256_hex(&bytes),
            b.id,
            "re-hashed"
        );
        assert_eq!(bytes.len() as u64, b.size);
    }
    let body = format::read_blob(&dir, request.blob_id.as_deref().unwrap()).unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert!(
        body["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("<REDACTED:anthropic_key:1>")
    );
    assert!(
        body["system"][1]["text"].as_str().unwrap().contains("<WS>"),
        "the workspace block names the root as <WS>"
    );

    // Imported, the redacted bodies still pass replay-check: the hashes
    // name the redacted bytes, and the messages were redacted the same.
    let other_dir = tempfile::tempdir().unwrap();
    let other = fresh_store(other_dir.path());
    let report = import(
        &other,
        &ImportOptions {
            path: dir.clone(),
            into_workspace: None,
            keep_ids: true,
        },
    )
    .unwrap();
    assert!(report.redacted);
    let copied = other.get_session(&session).unwrap();
    assert_eq!(copied.config["import"]["redacted"], true);
    assert_eq!(copied.workspace_id, None, "`<WS>` is no directory here");
    assert_eq!(copied.workspace_path.as_deref(), Some("<WS>"));
    assert!(other.list_workspaces().unwrap().is_empty());
    let replay = replay_check(&other, &ReplaySelection::default(), true).unwrap();
    assert_eq!((replay.checked, replay.passed), (3, 3), "{replay:#?}");
    assert!(other.integrity_check().unwrap().is_ok());

    // Without the pass, the secrets go out as they are.
    let plain = out.path().join("plain");
    let (_, manifest) = export(store, &opts(plain.clone(), select(&[&session]), None)).unwrap();
    assert!(manifest.redaction.is_none());
    assert!(
        files_of(&plain)
            .iter()
            .any(|(_, b)| String::from_utf8_lossy(b).contains(KEY))
    );
}

#[tokio::test]
async fn a_corrupted_bundle_blob_is_refused_by_id() {
    let h = Harness::new().await;
    let session = h.trajectory("add hello").await;
    let out = tempfile::tempdir().unwrap();
    let dir = out.path().join("bundle");
    export(h.store(), &opts(dir.clone(), select(&[&session]), None)).unwrap();
    let rows = format::Rows::read(&dir).unwrap();
    let victim = rows.blobs[2].id.clone();
    std::fs::write(format::blob_path(&dir, &victim), b"garbage").unwrap();

    let other_dir = tempfile::tempdir().unwrap();
    let other = fresh_store(other_dir.path());
    let err = import(
        &other,
        &ImportOptions {
            path: dir.clone(),
            into_workspace: None,
            keep_ids: true,
        },
    )
    .unwrap_err();
    match &err {
        BundleError::BlobCorrupted { id, .. } => assert_eq!(*id, victim),
        other => panic!("{other}"),
    }
    assert!(err.to_string().contains(&victim));
    assert_eq!(
        other
            .count_sessions(apprentice_core::trace::SessionStatus::Open)
            .unwrap(),
        0
    );

    std::fs::remove_file(format::blob_path(&dir, &victim)).unwrap();
    let err = import(
        &other,
        &ImportOptions {
            path: dir.clone(),
            into_workspace: None,
            keep_ids: true,
        },
    )
    .unwrap_err();
    assert!(
        matches!(&err, BundleError::BlobMissing { id } if *id == victim),
        "{err}"
    );

    // Not a bundle at all, and one from the future.
    let err = import(
        &other,
        &ImportOptions {
            path: out.path().to_path_buf(),
            into_workspace: None,
            keep_ids: false,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("no manifest.json"), "{err}");
    let future = out.path().join("future");
    std::fs::create_dir_all(&future).unwrap();
    std::fs::write(
        future.join("manifest.json"),
        r#"{"format_version": 2, "created_at": "t", "harness_version": "9", "schema_version": 3, "sessions": [], "counts": {"sessions":0,"workspaces":0,"agents":0,"steps":0,"events":0,"mentor_calls":0,"messages":0,"blobs":0,"blob_bytes":0}}"#,
    )
    .unwrap();
    let err = import(
        &other,
        &ImportOptions {
            path: future,
            into_workspace: None,
            keep_ids: false,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("format v2"), "{err}");
}

/// A store with two sessions made by hand (no mentor): one per day, one
/// on a workspace.
fn two_sessions(store: &TraceStore) -> (SessionId, SessionId, String) {
    let ws = store.add_workspace("C:/src/demo", None).unwrap();
    let a = store
        .create_session(&NewSession {
            title: Some("alpha".into()),
            workspace_path: Some("C:/src/demo".into()),
            workspace_id: Some(ws.id.clone()),
            config: json!({}),
        })
        .unwrap();
    let b = store
        .create_session(&NewSession {
            title: Some("beta".into()),
            workspace_path: None,
            workspace_id: None,
            config: json!({}),
        })
        .unwrap();
    (a, b, ws.id.to_string())
}

#[tokio::test]
async fn the_rpc_methods_answer_over_a_router() {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let store = fresh_store(&data);
    let (a, _b, ws) = two_sessions(&store);
    let out = tempfile::tempdir().unwrap();

    let mut router = Router::new(RouterConfig {
        daemon_version: "0".into(),
        pid: 1,
        token: None,
    });
    Arc::new(BundleService::new(
        Arc::clone(&store),
        home.path().to_path_buf(),
    ))
    .register(&mut router);
    assert_eq!(
        router.methods(),
        vec!["trace.export", "trace.import", "trace.replay_check"]
    );
    let (server_side, client_side) = tokio::io::duplex(1 << 16);
    let (sr, sw) = tokio::io::split(server_side);
    let router = Arc::new(router);
    tokio::spawn(async move {
        let _ = router.serve(sr, sw).await;
    });
    let (cr, cw) = tokio::io::split(client_side);
    let client = DaemonClient::from_streams(cr, cw, ClientOptions::default());
    client.hello("test", "0", None).await.unwrap();

    let bundle = out.path().join("ws.tar.zst");
    let r = client
        .call::<TraceExport>(TraceExportParams {
            output: bundle.to_string_lossy().into_owned(),
            session_ids: vec![],
            workspace_id: Some(ws.clone()),
            since: None,
            until: None,
            all: false,
            redact: true,
            redact_paths: false,
        })
        .await
        .unwrap();
    assert_eq!(r.manifest.counts.sessions, 1);
    assert_eq!(r.manifest.sessions[0].id, a.to_string());
    assert_eq!(
        r.manifest.selection.workspace_id.as_deref(),
        Some(ws.as_str())
    );
    assert!(r.manifest.redaction.is_some());
    assert!(!r.manifest.redaction.as_ref().unwrap().applied);
    assert!(Path::new(&r.path).is_file());

    // A relative path is refused: the daemon's cwd is not the caller's.
    let err = client
        .call::<TraceExport>(TraceExportParams {
            output: "relative.tar.zst".into(),
            session_ids: vec![],
            workspace_id: None,
            since: None,
            until: None,
            all: true,
            redact: true,
            redact_paths: false,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == codes::INVALID_PARAMS),
        "{err}"
    );
    let err = client
        .call::<TraceExport>(TraceExportParams {
            output: out.path().join("x").to_string_lossy().into_owned(),
            session_ids: vec![],
            workspace_id: None,
            since: Some("yesterday".into()),
            until: None,
            all: false,
            redact: true,
            redact_paths: false,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == codes::INVALID_PARAMS),
        "{err}"
    );

    let r = client
        .call::<TraceImport>(TraceImportParams {
            path: bundle.to_string_lossy().into_owned(),
            into_workspace: None,
            keep_ids: false,
        })
        .await
        .unwrap();
    assert_eq!(r.sessions.len(), 1);
    assert_ne!(r.sessions[0].to, a.to_string(), "the id is taken here");
    assert_eq!(r.counts.sessions, 1);
    let err = client
        .call::<TraceImport>(TraceImportParams {
            path: bundle.to_string_lossy().into_owned(),
            into_workspace: None,
            keep_ids: true,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == codes::CONFLICT),
        "{err}"
    );
    let err = client
        .call::<TraceImport>(TraceImportParams {
            path: out
                .path()
                .join("nope.tar.zst")
                .to_string_lossy()
                .into_owned(),
            into_workspace: None,
            keep_ids: true,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == codes::NOT_FOUND),
        "{err}"
    );

    let r = client
        .call::<TraceReplayCheck>(TraceReplayCheckParams {
            session_id: Some(a.to_string()),
            ..TraceReplayCheckParams::default()
        })
        .await
        .unwrap();
    assert_eq!(r.checked, 0, "no calls in a hand-made session");
    let err = client
        .call::<TraceReplayCheck>(TraceReplayCheckParams {
            call_id: Some("nope".into()),
            ..TraceReplayCheckParams::default()
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == codes::NOT_FOUND),
        "{err}"
    );

    // What `--json` prints parses back unchanged.
    let text = serde_json::to_string_pretty(&r).unwrap();
    assert_eq!(
        serde_json::from_str::<apprentice_api::types::ReplayReport>(&text).unwrap(),
        r
    );
}
