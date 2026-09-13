//! JSON shape snapshots for the wire types. A change here is a wire-protocol
//! change: review the diff and bump `API_VERSION` if it is not additive.

use apprentice_api::API_VERSION;
use apprentice_api::events::{
    AgentStatus, Event, EventNotification, LogLevel, Risk, StepPhase, ToolStream,
};
use apprentice_api::jsonrpc::{Id, Message, Response, RpcError};
use apprentice_api::methods::{
    AgentRunParams, AgentRunResult, ConfigGetParams, ConfigSetParams, HelloParams, HelloResult,
    PermissionRespondParams, StatsRepriceParams, StatsRepriceResult, StatsTokensParams,
    ToolsListParams, ToolsListResult, ToolsRuleParams, ToolsRuleResult, ToolsRulesParams,
    ToolsRulesResult, TraceGetParams, TraceListParams, WorkspaceAddParams, WorkspaceIdParams,
    WorkspaceInfoResult, WorkspaceListResult, WorkspaceRemoveResult,
};
use apprentice_api::types::{
    ApprenticeStats, ConfigLayer, Effort, PermissionAnswer, PermissionDecision, PermissionMode,
    PermissionSource, RuleDefault, RuleEffect, RuleFileInfo, RuleInfo, RuleMatch, RuleSource,
    RuleSpec, RunOptions, StatsRange, TokenBucket, TokenStats, ToolInfo, Usage, WorkspaceSummary,
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
                apprentice: Some(false),
                permission_mode: None,
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
fn permission_shapes() {
    assert_json_snapshot!(
        "permissions",
        (
            PermissionRespondParams {
                request_id: "p1".into(),
                answer: PermissionAnswer::AllowWorkspace,
                rule: Some(RuleSpec {
                    tool: "shell".into(),
                    effect: RuleEffect::Allow,
                    r#match: RuleMatch {
                        command_prefix: Some("cargo".into()),
                        ..RuleMatch::default()
                    },
                }),
            },
            ToolsRulesParams {
                workspace: Some("/work/repo".into()),
            },
            ToolsRulesResult {
                rules: vec![
                    RuleInfo {
                        source: RuleSource::Workspace,
                        index: 1,
                        line: Some(3),
                        name: None,
                        rule: RuleSpec {
                            tool: "write_file".into(),
                            effect: RuleEffect::Deny,
                            r#match: RuleMatch {
                                path: Some("secrets/**".into()),
                                outside_workspace: Some(false),
                                ..RuleMatch::default()
                            },
                        },
                    },
                    RuleInfo {
                        source: RuleSource::Builtin,
                        index: 1,
                        line: None,
                        name: Some("read_only_inside".into()),
                        rule: RuleSpec {
                            tool: "*".into(),
                            effect: RuleEffect::Allow,
                            r#match: RuleMatch {
                                risk: Some(Risk::ReadOnly),
                                outside_workspace: Some(false),
                                ..RuleMatch::default()
                            },
                        },
                    },
                ],
                files: vec![
                    RuleFileInfo {
                        source: RuleSource::Workspace,
                        path: "/work/repo/.harness/permissions.toml".into(),
                        exists: true,
                        default: Some(RuleDefault::Ask),
                        error: None,
                    },
                    RuleFileInfo {
                        source: RuleSource::User,
                        path: "/home/u/.config/apprentice-harness/permissions.toml".into(),
                        exists: true,
                        default: None,
                        error: Some(
                            "/home/u/.config/apprentice-harness/permissions.toml:4: invalid string"
                                .into()
                        ),
                    },
                ],
            },
            ToolsRuleParams {
                tool: "shell".into(),
                r#match: RuleMatch {
                    command_regex: Some(r"\bgit\s+push\b".into()),
                    ..RuleMatch::default()
                },
                layer: ConfigLayer::User,
                workspace: None,
            },
            ToolsRuleResult {
                path: "/home/u/.config/apprentice-harness/permissions.toml".into(),
                line: 9,
                rule: RuleSpec {
                    tool: "shell".into(),
                    effect: RuleEffect::Deny,
                    r#match: RuleMatch {
                        command_regex: Some(r"\bgit\s+push\b".into()),
                        ..RuleMatch::default()
                    },
                },
            },
            RunOptions {
                permission_mode: Some(PermissionMode::Plan),
                ..RunOptions::default()
            },
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
        Event::AgentStep {
            agent_id: "a1".into(),
            seq: 2,
            phase: StepPhase::Tools,
        },
        Event::AgentToolCall {
            agent_id: "a1".into(),
            call_id: "c1".into(),
            name: "read_file".into(),
            input: json!({"path": "src/main.rs"}),
        },
        Event::AgentToolProgress {
            agent_id: "a1".into(),
            call_id: "c3".into(),
            stream: ToolStream::Stderr,
            text: "   Compiling core v0.1.0\n".into(),
        },
        Event::AgentToolResult {
            agent_id: "a1".into(),
            call_id: "c1".into(),
            name: "read_file".into(),
            ok: true,
            summary: "read 12 lines".into(),
            blob_id: Some("abc".into()),
            mentor_bytes: 412,
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
            session_usage: Usage {
                input_tokens: 2408,
                output_tokens: 620,
                cache_read_input_tokens: 900,
                cache_creation_input_tokens: 900,
            },
            session_cost_usd: Some(0.0276),
        },
        Event::AgentWarning {
            agent_id: "a1".into(),
            kind: "context_large".into(),
            message: "the last call used 612000 input tokens (soft limit 600000)".into(),
        },
        Event::AgentWaiting {
            agent_id: "a1".into(),
            reason: "rate_limited".into(),
            until: "2026-09-12T10:00:30.000Z".into(),
            wait_ms: 30_000,
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
            description: "shell: cargo test".into(),
            command: Some("cargo test".into()),
            paths: vec![".".into()],
            suggested_rules: vec![RuleSpec {
                tool: "shell".into(),
                effect: RuleEffect::Allow,
                r#match: RuleMatch {
                    command_prefix: Some("cargo test".into()),
                    ..RuleMatch::default()
                },
            }],
            timeout_s: 600,
        },
        Event::PermissionDecision {
            agent_id: "a1".into(),
            call_id: "c2".into(),
            tool: "shell".into(),
            decision: PermissionDecision::Deny,
            source: PermissionSource::User,
            request_id: Some("p1".into()),
            rule_ref: None,
            reason: Some("denied by user".into()),
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
