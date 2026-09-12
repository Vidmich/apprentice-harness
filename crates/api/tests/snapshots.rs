//! JSON shape snapshots for the wire types. A change here is a wire-protocol
//! change: review the diff and bump `API_VERSION` if it is not additive.

use apprentice_api::API_VERSION;
use apprentice_api::events::{AgentStatus, Event, EventNotification, LogLevel, Risk};
use apprentice_api::jsonrpc::{Id, Message, Response, RpcError};
use apprentice_api::methods::{
    AgentRunParams, AgentRunResult, ConfigGetParams, ConfigSetParams, HelloParams, HelloResult,
    StatsRepriceParams, StatsRepriceResult, StatsTokensParams, ToolsListParams, ToolsListResult,
    TraceGetParams, TraceListParams, WorkspaceAddParams, WorkspaceIdParams, WorkspaceInfoResult,
    WorkspaceListResult, WorkspaceRemoveResult,
};
use apprentice_api::types::{
    ApprenticeStats, ConfigLayer, Effort, RunOptions, StatsRange, TokenBucket, TokenStats,
    ToolInfo, Usage, WorkspaceSummary,
};
use insta::assert_json_snapshot;
use serde_json::json;

#[test]
fn request_and_response_shapes() {
    let req = Message::request(
        Id::Number(1),
        "daemon.hello",
        Some(
            serde_json::to_value(HelloParams {
                client: "harness-cli".into(),
                client_version: "0.1.0".into(),
                api_version: API_VERSION,
                token: Some("tok".into()),
            })
            .unwrap(),
        ),
    );
    assert_json_snapshot!("hello_request", req);

    let ok = Response::success(
        Id::Number(1),
        serde_json::to_value(HelloResult {
            daemon_version: "0.1.0".into(),
            api_version: 1,
            pid: 1234,
        })
        .unwrap(),
    );
    assert_json_snapshot!("hello_response", ok);

    let err = Response::failure(Some(Id::Number(2)), RpcError::incompatible_api(1, 2));
    assert_json_snapshot!("error_response", err);

    let parse = Response::failure(None, RpcError::parse_error("expected value at line 1"));
    assert_json_snapshot!("parse_error_response", parse);
}

#[test]
fn method_param_shapes() {
    assert_json_snapshot!(
        "agent_run_params",
        AgentRunParams {
            session_id: "s1".into(),
            prompt: "hello".into(),
            options: RunOptions {
                model: None,
                effort: Some(Effort::XHigh),
                apprentice: Some(false)
            },
        }
    );
    assert_json_snapshot!(
        "agent_run_result",
        AgentRunResult {
            agent_id: "a1".into(),
            subscription: "a1".into()
        }
    );
    assert_json_snapshot!("config_get_params_default", ConfigGetParams::default());
    assert_json_snapshot!(
        "config_set_params",
        ConfigSetParams {
            key: "mentor.effort".into(),
            value: json!("high"),
            layer: ConfigLayer::Workspace,
            workspace: Some("C:/src/foo".into()),
        }
    );
    assert_json_snapshot!(
        "trace_list_params",
        TraceListParams {
            session_id: Some("s1".into()),
            agent_id: None,
            kinds: vec!["mentor.request".into(), "mentor.response".into()],
            limit: Some(50),
            before_seq: None,
        }
    );
    assert_json_snapshot!(
        "trace_get_params",
        TraceGetParams {
            event_id: "e1".into(),
            include_blob: true
        }
    );
    assert_json_snapshot!("stats_tokens_params_default", StatsTokensParams::default());
}

#[test]
fn stats_shape() {
    let bucket = |key: Option<&str>| TokenBucket {
        key: key.map(str::to_owned),
        label: None,
        calls: 14,
        input: 182_340,
        output: 21_004,
        cache_read: 610_222,
        cache_creation: 12_000,
        cost_usd: 1.84,
        unpriced_calls: 0,
    };
    assert_json_snapshot!(
        "token_stats",
        TokenStats {
            range: StatsRange {
                since: Some("2026-09-01T00:00:00Z".into()),
                until: None
            },
            tz: "Europe/Amsterdam".into(),
            totals: bucket(None),
            by_model: vec![bucket(Some("claude-opus-5"))],
            by_day: vec![bucket(Some("2026-09-11"))],
            by_session: vec![],
            apprentice: ApprenticeStats::default(),
        }
    );
    assert_json_snapshot!(
        "stats_reprice",
        (
            StatsRepriceParams {
                model: Some("claude-opus-5".into()),
                since: Some("7d".into()),
                ..StatsRepriceParams::default()
            },
            StatsRepriceResult {
                examined: 4,
                changed: 2,
                unpriced: 1
            }
        )
    );
}

#[test]
fn tools_shape() {
    assert_json_snapshot!(
        "tools_list",
        (
            ToolsListParams {
                workspace: Some("/work/repo".into())
            },
            ToolsListResult {
                tools: vec![
                    ToolInfo {
                        name: "read_file".into(),
                        description: "Read a file from the workspace.".into(),
                        input_schema: json!({
                            "type": "object",
                            "properties": {"path": {"type": "string"}},
                            "required": ["path"]
                        }),
                        risk: Risk::ReadOnly,
                        tags: vec!["files".into()],
                        timeout_s: None,
                        enabled: true,
                    },
                    ToolInfo {
                        name: "shell".into(),
                        description: "Run a command.".into(),
                        input_schema: json!({"type": "object"}),
                        risk: Risk::Execute,
                        tags: vec![],
                        timeout_s: Some(900),
                        enabled: false,
                    }
                ]
            }
        )
    );
}

#[test]
fn workspace_shapes() {
    let summary = WorkspaceSummary {
        id: "w1".into(),
        root: r"C:\src\repo".into(),
        name: "repo".into(),
        created_at: "2026-09-12T10:00:00.000Z".into(),
        last_used_at: "2026-09-12T11:00:00.000Z".into(),
    };
    assert_json_snapshot!(
        "workspace_add",
        (
            WorkspaceAddParams {
                root: "C:/src/repo".into(),
                name: None
            },
            summary.clone()
        )
    );
    assert_json_snapshot!(
        "workspace_list",
        WorkspaceListResult {
            workspaces: vec![summary]
        }
    );
    assert_json_snapshot!(
        "workspace_remove",
        (
            WorkspaceIdParams { id: "w1".into() },
            WorkspaceRemoveResult {
                sessions_unlinked: 2
            }
        )
    );
    assert_json_snapshot!(
        "workspace_info",
        WorkspaceInfoResult {
            id: "w1".into(),
            root: "/src/repo".into(),
            name: "repo".into(),
            created_at: "2026-09-12T10:00:00.000Z".into(),
            last_used_at: "2026-09-12T11:00:00.000Z".into(),
            file_count: 1234,
            index_truncated: false,
            index_age_s: 3,
            git_head: Some("0123456789abcdef0123456789abcdef01234567".into()),
            git_branch: Some("main".into()),
            has_instructions: true,
            has_config: false,
            has_ignore_file: true,
            config_overrides: vec!["mentor.effort".into()],
        }
    );
}

#[test]
fn event_shapes() {
    let events = vec![
        Event::AgentStarted {
            agent_id: "a1".into(),
            session_id: "s1".into(),
        },
        Event::AgentTextDelta {
            agent_id: "a1".into(),
            text: "Hel".into(),
        },
        Event::AgentThinkingDelta {
            agent_id: "a1".into(),
            text: "hmm".into(),
        },
        Event::AgentToolCall {
            agent_id: "a1".into(),
            call_id: "c1".into(),
            name: "read_file".into(),
            input: json!({"path": "src/main.rs"}),
        },
        Event::AgentToolResult {
            agent_id: "a1".into(),
            call_id: "c1".into(),
            ok: true,
            summary: "read 12 lines".into(),
            blob_id: Some("abc".into()),
        },
        Event::AgentUsage {
            agent_id: "a1".into(),
            call_id: "m1".into(),
            usage: Usage {
                input_tokens: 1204,
                output_tokens: 310,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 900,
            },
            cost_usd: Some(0.0138),
        },
        Event::AgentFinished {
            agent_id: "a1".into(),
            status: AgentStatus::Error,
            error: Some(RpcError::cancelled()),
            truncated: false,
        },
        Event::PermissionRequest {
            request_id: "p1".into(),
            agent_id: "a1".into(),
            tool: "shell".into(),
            input: json!({"command": "cargo test"}),
            risk: Risk::Execute,
        },
        Event::Log {
            level: LogLevel::Warn,
            message: "3 event(s) dropped".into(),
        },
    ];
    let notifications: Vec<_> = events
        .into_iter()
        .enumerate()
        .map(|(i, event)| EventNotification {
            subscription: "a1".into(),
            seq: i as u64 + 1,
            event,
        })
        .collect();
    assert_json_snapshot!("events", notifications);
}
