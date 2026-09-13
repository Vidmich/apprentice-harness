//! The permission engine (task M01-07) over RPC: `tools.rules`,
//! `tools.allow`, `tools.deny`, and the ask flow — a `permission.request`
//! event answered with `permission.respond` lets the tool proceed, a
//! denial reaches the mentor as an error result, and every decision is
//! a `permission.decision` event in the trace.

use std::sync::Arc;
use std::time::Duration;

use apprentice_api::events::Event;
use apprentice_api::jsonrpc::codes;
use apprentice_api::methods::{
    PermissionRespond, PermissionRespondParams, SessionCreateParams, ToolsAllow, ToolsDeny,
    ToolsRuleParams, ToolsRules, ToolsRulesParams,
};
use apprentice_api::server::{Router, RouterConfig};
use apprentice_api::types::{
    ConfigLayer, PermissionAnswer, PermissionMode, RuleEffect, RuleMatch, RuleSource, TraceEvent,
};
use apprentice_client::{ClientError, ClientOptions, DaemonClient};
use apprentice_core::app::AppState;
use apprentice_core::config::{Config, ConfigLoader, Paths, SecretStoreKind};
use apprentice_core::permissions::{AgentPrompter, Engine, PermissionGate};
use apprentice_core::tools::{Executor, SeenFiles, ToolCall, ToolResultKind};
use apprentice_core::trace::{NewAgent, SessionId, StepRef, kinds};
use apprentice_core::workspace::Workspace;
use serde_json::json;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

struct Harness {
    _home: tempfile::TempDir,
    _repo: tempfile::TempDir,
    state: Arc<AppState>,
    workspace: Arc<Workspace>,
    client: DaemonClient,
    _server: tokio::task::JoinHandle<()>,
}

impl Harness {
    async fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        std::fs::write(repo.path().join("src/a.rs"), "fn a() {}\n").unwrap();
        let paths = Paths::from_home(home.path());
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        std::fs::write(
            paths.config_file(),
            "[daemon]\nsecret_store = \"file\"\n\n[permissions]\nask_timeout_s = 2\n",
        )
        .unwrap();
        let loader = ConfigLoader::new(paths);
        let mut config = Config::default();
        config.daemon.secret_store = SecretStoreKind::File;
        let state = AppState::open_with(loader, &config).unwrap();
        let workspace = Arc::new(Workspace::open(repo.path()).unwrap());

        let mut router = Router::new(RouterConfig {
            daemon_version: "0".into(),
            pid: 1,
            token: None,
        });
        state.register(&mut router);
        let router = Arc::new(router);
        let (server_side, client_side) = tokio::io::duplex(1 << 20);
        let (sr, sw) = tokio::io::split(server_side);
        let server = tokio::spawn(async move {
            let _ = router.serve(sr, sw).await;
        });
        let (cr, cw) = tokio::io::split(client_side);
        let client = DaemonClient::from_streams(cr, cw, ClientOptions::default());
        client.hello("permissions-test", "0", None).await.unwrap();
        Self {
            _home: home,
            _repo: repo,
            state,
            workspace,
            client,
            _server: server,
        }
    }

    fn root(&self) -> String {
        self.workspace.root_string()
    }
}

#[tokio::test]
async fn rules_are_listed_and_added_over_rpc() {
    let h = Harness::new().await;

    // Nothing but the built-ins at first; both files missing.
    let r = h
        .client
        .call::<ToolsRules>(ToolsRulesParams {
            workspace: Some(h.root()),
        })
        .await
        .unwrap();
    assert!(r.rules.iter().all(|r| r.source == RuleSource::Builtin));
    assert_eq!(r.rules[0].name.as_deref(), Some("read_only_inside"));
    assert_eq!(r.files.len(), 2);
    assert_eq!(r.files[0].source, RuleSource::Workspace);
    assert!(!r.files[0].exists);
    assert!(r.files[0].path.ends_with("permissions.toml"));
    assert_eq!(r.files[1].source, RuleSource::User);
    assert!(!r.files[1].exists);

    // tools.allow into the workspace file.
    let added = h
        .client
        .call::<ToolsAllow>(ToolsRuleParams {
            tool: "shell".into(),
            r#match: RuleMatch {
                command_prefix: Some("cargo test".into()),
                ..RuleMatch::default()
            },
            layer: ConfigLayer::Workspace,
            workspace: Some(h.root()),
        })
        .await
        .unwrap();
    assert_eq!(added.path, h.workspace.permissions_file().to_string_lossy());
    assert_eq!(added.line, 2);
    assert_eq!(added.rule.effect, RuleEffect::Allow);
    let text = std::fs::read_to_string(h.workspace.permissions_file()).unwrap();
    assert!(
        text.contains("# added with `harness tools`\n[[rule]]\n"),
        "{text}"
    );

    // tools.deny into the user file.
    let denied = h
        .client
        .call::<ToolsDeny>(ToolsRuleParams {
            tool: "read_file".into(),
            r#match: RuleMatch {
                outside_workspace: Some(true),
                ..RuleMatch::default()
            },
            layer: ConfigLayer::User,
            workspace: None,
        })
        .await
        .unwrap();
    assert_eq!(
        denied.path,
        h.state
            .paths()
            .config_dir
            .join("permissions.toml")
            .to_string_lossy()
    );
    assert_eq!(denied.rule.effect, RuleEffect::Deny);

    let r = h
        .client
        .call::<ToolsRules>(ToolsRulesParams {
            workspace: Some(h.root()),
        })
        .await
        .unwrap();
    let heads: Vec<(RuleSource, usize, Option<u64>)> = r
        .rules
        .iter()
        .map(|r| (r.source, r.index, r.line))
        .collect();
    assert_eq!(heads[0], (RuleSource::Workspace, 1, Some(2)));
    assert_eq!(heads[1], (RuleSource::User, 1, Some(2)));
    assert_eq!(heads[2], (RuleSource::Builtin, 1, None));
    assert!(r.files.iter().all(|f| f.exists && f.error.is_none()));

    // The workspace layer needs a workspace; a broken file is reported
    // with its line and refused for appends.
    let err = h
        .client
        .call::<ToolsAllow>(ToolsRuleParams {
            tool: "shell".into(),
            r#match: RuleMatch::default(),
            layer: ConfigLayer::Workspace,
            workspace: None,
        })
        .await
        .unwrap_err();
    let ClientError::Rpc(e) = err else {
        panic!("{err}")
    };
    assert_eq!(e.code, codes::INVALID_PARAMS);
    std::fs::write(h.workspace.permissions_file(), "default = \"maybe\"\n").unwrap();
    let r = h
        .client
        .call::<ToolsRules>(ToolsRulesParams {
            workspace: Some(h.root()),
        })
        .await
        .unwrap();
    assert!(
        r.files[0]
            .error
            .as_deref()
            .unwrap()
            .contains("permissions.toml:1:")
    );
    assert!(r.rules.iter().all(|r| r.source != RuleSource::Workspace));
    let err = h
        .client
        .call::<ToolsAllow>(ToolsRuleParams {
            tool: "shell".into(),
            r#match: RuleMatch::default(),
            layer: ConfigLayer::Workspace,
            workspace: Some(h.root()),
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("permissions.toml:1:"), "{err}");

    // Answering a request nobody made.
    let err = h
        .client
        .call::<PermissionRespond>(PermissionRespondParams {
            request_id: "nope".into(),
            answer: PermissionAnswer::AllowOnce,
            rule: None,
        })
        .await
        .unwrap_err();
    let ClientError::Rpc(e) = err else {
        panic!("{err}")
    };
    assert_eq!(e.kind(), Some("not_found"));
}

#[tokio::test]
async fn a_prompt_answered_over_rpc_lets_the_tool_run() {
    let h = Harness::new().await;
    let session: SessionId = h
        .state
        .session_create(&SessionCreateParams {
            workspace: Some(h.root()),
            title: None,
        })
        .await
        .unwrap()
        .session_id
        .into();
    let agent = h
        .state
        .writer()
        .run({
            let session = session.clone();
            move |store| store.start_agent(&NewAgent::main(session, "task"))
        })
        .await
        .unwrap();
    let step = h
        .state
        .writer()
        .run({
            let agent = agent.clone();
            move |store| store.start_step(&agent)
        })
        .await
        .unwrap();
    let at = StepRef {
        session: session.clone(),
        agent: agent.clone(),
        step,
    };
    let config = h
        .state
        .loader()
        .load(Some(h.workspace.root()))
        .unwrap()
        .config;
    let engine = Arc::new(Engine::new(
        h.state.paths(),
        Some(&h.workspace),
        config.permissions.clone(),
        h.state.permissions().session_rules(&session),
    ));
    let (events, mut live) = broadcast::channel::<Event>(64);
    let prompter = Arc::new(AgentPrompter::new(
        Arc::clone(h.state.permissions()),
        Arc::new(events.clone()),
    ));
    let gate = PermissionGate::new(
        Arc::clone(&engine),
        prompter,
        h.state.writer(),
        at.clone(),
        PermissionMode::Default,
    )
    .with_sink(Arc::new(events.clone()));
    let executor = Executor::new(
        h.state.tools(),
        &gate,
        h.state.writer(),
        &config.tools,
        at.clone(),
        CancellationToken::new(),
    )
    .with_workspace(Some(Arc::clone(&h.workspace)))
    .with_seen_files(Arc::new(SeenFiles::new()));

    // The client answers each request it sees: deny, then allow for the
    // session, then nothing (timeout).
    let client = &h.client;
    let answers = [
        Some(PermissionAnswer::DenyOnce),
        Some(PermissionAnswer::AllowSession),
        None,
    ];
    let answering = async {
        let mut requests = Vec::new();
        let mut decisions = Vec::new();
        while requests.len() < 3 || decisions.len() < 4 {
            let ev = tokio::time::timeout(Duration::from_secs(10), live.recv())
                .await
                .expect("an event in time")
                .unwrap();
            match ev {
                Event::PermissionRequest {
                    request_id,
                    tool,
                    paths,
                    timeout_s,
                    ..
                } => {
                    assert_eq!(tool, "write_file");
                    assert_eq!(timeout_s, 2);
                    if let Some(answer) = answers[requests.len()] {
                        client
                            .call::<PermissionRespond>(PermissionRespondParams {
                                request_id: request_id.clone(),
                                answer,
                                rule: None,
                            })
                            .await
                            .unwrap();
                    }
                    requests.push((request_id, paths));
                }
                Event::PermissionDecision {
                    call_id,
                    decision,
                    source,
                    request_id,
                    ..
                } => decisions.push((call_id, decision, source, request_id)),
                _ => {}
            }
        }
        (requests, decisions)
    };
    let calls = async {
        let mut out = Vec::new();
        for (id, path) in [
            ("c1", "src/a.rs"),
            ("c2", "src/a.rs"),
            ("c3", "src/b.rs"),
            ("c4", "../x"),
        ] {
            out.push(
                executor
                    .execute_one(ToolCall::new(
                        id,
                        "write_file",
                        json!({"path": path, "content": "x\n"}),
                    ))
                    .await,
            );
        }
        out
    };
    let ((requests, decisions), results) = tokio::join!(answering, calls);

    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].1, ["src/a.rs"]);
    // c1 denied by the user; c2 allowed for the session; c3 matches the
    // session rule (src/**) without a prompt; c4 is outside: asked, and
    // nobody answers.
    let kinds_: Vec<ToolResultKind> = results.iter().map(|r| r.kind).collect();
    assert_eq!(
        kinds_,
        [
            ToolResultKind::Denied,
            ToolResultKind::Ok,
            ToolResultKind::Ok,
            ToolResultKind::Denied
        ],
        "{results:#?}"
    );
    let denied = serde_json::to_value(&results[0].block).unwrap();
    assert_eq!(denied["is_error"], true);
    assert_eq!(denied["content"][0]["text"], "denied by user");
    let timed_out = serde_json::to_value(&results[3].block).unwrap();
    assert!(
        timed_out["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("nobody answered the permission request within 2 s"),
        "{timed_out}"
    );
    assert_eq!(
        std::fs::read_to_string(h.workspace.root().join("src/a.rs")).unwrap(),
        "x\n"
    );
    assert!(h.workspace.root().join("src/b.rs").is_file());
    assert_eq!(decisions.len(), 4);
    assert_eq!(decisions[0].0, "c1");
    assert_eq!(decisions[0].3.as_deref(), Some(requests[0].0.as_str()));
    assert_eq!(
        decisions[2].2,
        apprentice_api::types::PermissionSource::Session
    );
    assert_eq!(
        decisions[3].2,
        apprentice_api::types::PermissionSource::Timeout
    );
    assert!(h.state.permissions().pending().is_empty());

    // Every decision is in the trace, in call order.
    h.state.writer().flush().await.unwrap();
    let events: Vec<TraceEvent> = h.state.store().session_events(&session).unwrap();
    let recorded: Vec<(String, String, String)> = events
        .iter()
        .filter(|e| e.summary.kind == kinds::PERMISSION_DECISION)
        .map(|e| {
            (
                e.payload["call_id"].as_str().unwrap().to_owned(),
                e.payload["decision"].as_str().unwrap().to_owned(),
                e.payload["source"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        recorded,
        [
            ("c1".to_owned(), "deny".to_owned(), "user".to_owned()),
            ("c2".to_owned(), "allow".to_owned(), "user".to_owned()),
            ("c3".to_owned(), "allow".to_owned(), "session".to_owned()),
            ("c4".to_owned(), "deny".to_owned(), "timeout".to_owned()),
        ]
    );
    let c2 = events
        .iter()
        .find(|e| e.summary.kind == kinds::PERMISSION_DECISION && e.payload["call_id"] == "c2")
        .unwrap();
    assert_eq!(c2.payload["answer"], "allow_session");
    assert_eq!(c2.payload["rule_ref"], "session:1");
    assert_eq!(c2.payload["asked"], true);
    assert_eq!(c2.summary.agent_id.as_deref(), Some(agent.as_str()));
    let c4 = events
        .iter()
        .find(|e| e.summary.kind == kinds::PERMISSION_DECISION && e.payload["call_id"] == "c4")
        .unwrap();
    assert_eq!(c4.payload["outside_workspace"], true);
    assert!(c4.payload["waited_ms"].as_u64().unwrap() >= 1900);
    h.state.close().await;
}
