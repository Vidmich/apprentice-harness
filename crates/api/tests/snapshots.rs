//! JSON shape snapshots for the wire types. A change here is a wire-protocol
//! change: review the diff and bump `API_VERSION` if it is not additive.

use apprentice_api::API_VERSION;
use apprentice_api::events::{
    AgentStatus, Event, EventNotification, LogLevel, Risk, StepPhase, ToolStream,
};
use apprentice_api::jsonrpc::{Id, Message, Response, RpcError};
use apprentice_api::methods::{
    AgentRunParams, AgentRunResult, ConfigGetParams, ConfigSetParams, HelloParams, HelloResult,
    PermissionRespondParams, PromptBlock, PromptShowParams, PromptShowResult, SessionDeleteParams,
    SessionDeleteResult, SessionGetParams, SessionGetResult, SessionListParams, SessionListResult,
    SessionMarkParams, SessionMarkResult, SessionSearchParams, SessionSearchResult,
    StatsCallsParams, StatsCallsResult, StatsOutcomesParams, StatsRepriceParams,
    StatsRepriceResult, StatsTokensParams, ToolsListParams, ToolsListResult, ToolsRuleParams,
    ToolsRuleResult, ToolsRulesParams, ToolsRulesResult, TraceExportParams, TraceExportResult,
    TraceGetParams, TraceImportParams, TraceImportResult, TraceListParams, TraceReplayCheckParams,
    WorkspaceAddParams, WorkspaceIdParams, WorkspaceInfoResult, WorkspaceInitParams,
    WorkspaceInitResult, WorkspaceListResult, WorkspaceRemoveResult,
};
use apprentice_api::types::{
    AgentSummary, ApprenticeStats, BUNDLE_FORMAT_VERSION, BundleCounts, BundleManifest,
    BundleSelection, BundleSession, CallSummary, ConfigLayer, Effort, ImportedSession,
    MentorCallInfo, OutcomeInfo, OutcomeStats, PermissionAnswer, PermissionDecision,
    PermissionMode, PermissionSource, RedactionReport, RedactionRule, ReplayCall, ReplayReport,
    ReplayStatus, RuleDefault, RuleEffect, RuleFileInfo, RuleInfo, RuleMatch, RuleSource, RuleSpec,
    RunOptions, SESSION_EXPORT_FORMAT, SessionExport, SessionInfo, SessionMark, SessionMessage,
    SessionSearchHit, SessionSummary, StatsGroup, StatsRange, TokenBucket, TokenStats, ToolInfo,
    Usage, WorkspaceSummary,
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
    assert_json_snapshot!(
        "stats_tokens_params_grouped",
        StatsTokensParams {
            since: Some("7d".into()),
            workspace_id: Some("w1".into()),
            group_by: vec![StatsGroup::Day, StatsGroup::Workspace, StatsGroup::Kind],
            ..StatsTokensParams::default()
        }
    );
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
            by_workspace: vec![],
            by_kind: vec![bucket(Some("step"))],
            apprentice: ApprenticeStats::default(),
        }
    );
    assert_json_snapshot!(
        "stats_outcomes",
        (
            StatsOutcomesParams {
                since: Some("7d".into()),
                until: None,
                workspace_id: None,
            },
            OutcomeStats {
                range: StatsRange {
                    since: Some("2026-09-05T00:00:00Z".into()),
                    until: None,
                },
                agents: 20,
                labelled: 13,
                labelled_share: 0.65,
                by_kind: [
                    ("files_changed".to_owned(), 17),
                    ("tests".to_owned(), 11),
                    ("user_accept".to_owned(), 6),
                    ("error".to_owned(), 2),
                ]
                .into_iter()
                .collect(),
                tests_passed: 9,
                tests_failed: 2,
                accepted: 6,
                rejected: 1,
                done: 4,
                errors: [("max_iterations".to_owned(), 1), ("stalled".to_owned(), 1)]
                    .into_iter()
                    .collect(),
            }
        )
    );
    assert_json_snapshot!(
        "stats_calls",
        (
            StatsCallsParams {
                since: Some("2026-09-01".into()),
                session_id: Some("s1".into()),
                limit: Some(50),
                offset: 100,
                ..StatsCallsParams::default()
            },
            StatsCallsResult {
                calls: vec![
                    CallSummary {
                        call_id: "m2".into(),
                        started_at: "2026-09-12T10:04:20.000Z".into(),
                        session_id: "s1".into(),
                        session_title: Some("Add a hello module".into()),
                        workspace_id: Some("w1".into()),
                        agent_id: "a1".into(),
                        kind: "title".into(),
                        model: "claude-haiku-4-5-20251001".into(),
                        effort: None,
                        status: "ok".into(),
                        stop_reason: Some("end_turn".into()),
                        input: 80,
                        output: 6,
                        cache_read: 0,
                        cache_creation: 0,
                        cost_usd: Some(0.000_11),
                        total_ms: Some(640),
                        request_event_id: "e9".into(),
                    },
                    CallSummary {
                        call_id: "m1".into(),
                        started_at: "2026-09-12T10:04:00.000Z".into(),
                        session_id: "s1".into(),
                        session_title: Some("Add a hello module".into()),
                        workspace_id: Some("w1".into()),
                        agent_id: "a1".into(),
                        kind: "step".into(),
                        model: "claude-opus-5".into(),
                        effort: Some("high".into()),
                        status: "error".into(),
                        stop_reason: None,
                        input: 0,
                        output: 0,
                        cache_read: 0,
                        cache_creation: 0,
                        cost_usd: None,
                        total_ms: Some(12_030),
                        request_event_id: "e7".into(),
                    },
                ],
                total: 102,
            }
        )
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
fn prompt_shape() {
    assert_json_snapshot!(
        "prompt_show",
        (
            PromptShowParams {
                session_id: None,
                workspace: Some("/work/repo".into()),
                count: true,
            },
            PromptShowResult {
                version: "mentor_system_v1".into(),
                blocks: vec![
                    PromptBlock {
                        text: "You are the mentor model of apprentice-harness ...".into(),
                        cache: true,
                    },
                    PromptBlock {
                        text: "#workspace
root: /work/repo
"
                        .into(),
                        cache: true,
                    },
                ],
                session_id: None,
                workspace: Some("/work/repo".into()),
                tokens: Some(1042),
                token_error: None,
            }
        )
    );
}

#[test]
fn session_shapes() {
    let usage = Usage {
        input_tokens: 1204,
        output_tokens: 310,
        cache_read_input_tokens: 900,
        cache_creation_input_tokens: 0,
    };
    let summary = SessionSummary {
        id: "s1".into(),
        title: Some("Add a hello module".into()),
        workspace: Some("C:/src/repo".into()),
        workspace_id: Some("w1".into()),
        status: "open".into(),
        created_at: "2026-09-12T10:00:00.000Z".into(),
        updated_at: "2026-09-12T10:05:00.000Z".into(),
        message_count: 7,
        last_activity: "2026-09-12T10:04:30.000Z".into(),
        last_agent_status: Some(AgentStatus::Ok),
        running_agent: Some("a2".into()),
        usage,
        cost_usd: Some(0.0138),
        calls: 2,
    };
    assert_json_snapshot!(
        "session_list",
        (
            SessionListParams {
                query: Some("hello".into()),
                workspace: None,
                workspace_id: Some("w1".into()),
                include_archived: false,
                limit: Some(20),
                offset: None,
            },
            SessionListResult {
                sessions: vec![summary.clone()],
            }
        )
    );
    let info = SessionInfo {
        summary,
        title_source: Some("generated".into()),
        prompt_version: Some("mentor_system_v1".into()),
        tools_hash: Some("9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".into()),
        config: json!({ "mentor": { "model": "claude-opus-5" } }),
    };
    let messages = vec![
        SessionMessage {
            seq: 1,
            role: "user".into(),
            content: json!([{ "type": "text", "text": "add hello" }]),
            agent_id: Some("a1".into()),
            step_id: None,
            created_at: "2026-09-12T10:00:01.000Z".into(),
        },
        SessionMessage {
            seq: 2,
            role: "assistant".into(),
            content: json!([
                { "type": "thinking", "thinking": "…", "signature": "sig" },
                { "type": "text", "text": "I'll read both files." },
                { "type": "tool_use", "id": "toolu_01A", "name": "read_file", "input": { "path": "src/main.rs" } }
            ]),
            agent_id: Some("a1".into()),
            step_id: Some("st1".into()),
            created_at: "2026-09-12T10:00:03.000Z".into(),
        },
    ];
    assert_json_snapshot!(
        "session_get",
        (
            SessionGetParams {
                id: "s1".into(),
                after_seq: None,
                before_seq: Some(3),
                limit: Some(2),
            },
            SessionGetResult {
                session: info.clone(),
                messages: messages.clone(),
                has_more: true,
                agents: vec![AgentSummary {
                    id: "a1".into(),
                    status: "ok".into(),
                    started_at: "2026-09-12T10:00:01.000Z".into(),
                    ended_at: Some("2026-09-12T10:00:09.000Z".into()),
                    model: Some("claude-opus-5".into()),
                    calls: 2,
                    usage,
                    cost_usd: Some(0.0138),
                    error: None,
                    outcomes: vec![
                        OutcomeInfo {
                            event_id: "e20".into(),
                            kind: "tests".into(),
                            summary: "12 passed (cargo test)".into(),
                            ok: Some(true),
                            details: json!({
                                "runner": "cargo",
                                "passed": 12,
                                "failed": 0,
                                "skipped": 1,
                                "exit_code": 0,
                                "duration_ms": 2410,
                                "command": "cargo test",
                                "source": "run_tests",
                            }),
                            at: "2026-09-12T10:00:07.000Z".into(),
                        },
                        OutcomeInfo {
                            event_id: "e24".into(),
                            kind: "files_changed".into(),
                            summary: "1 file added".into(),
                            ok: None,
                            details: json!({
                                "changed": [],
                                "added": ["src/hello.rs"],
                                "deleted": [],
                                "counts": { "changed": 0, "added": 1, "deleted": 0 },
                            }),
                            at: "2026-09-12T10:00:09.000Z".into(),
                        },
                    ],
                }],
            }
        )
    );
    assert_json_snapshot!(
        "session_mark",
        (
            SessionMarkParams {
                id: "s1".into(),
                mark: SessionMark::Accept,
                note: Some("compiles and the tests pass".into()),
                agent_id: None,
            },
            SessionMarkResult {
                agent_id: "a1".into(),
                outcome: OutcomeInfo {
                    event_id: "e30".into(),
                    kind: "user_accept".into(),
                    summary: "accepted: compiles and the tests pass".into(),
                    ok: Some(true),
                    details: json!({ "note": "compiles and the tests pass" }),
                    at: "2026-09-12T10:06:00.000Z".into(),
                },
            }
        )
    );
    assert_json_snapshot!(
        "session_search",
        (
            SessionSearchParams {
                query: "hello".into(),
                include_archived: false,
                limit: None,
            },
            SessionSearchResult {
                hits: vec![SessionSearchHit {
                    session_id: "s1".into(),
                    title: Some("Add a hello module".into()),
                    workspace: Some("C:/src/repo".into()),
                    seq: 1,
                    role: "user".into(),
                    snippet: "add [hello]".into(),
                    created_at: "2026-09-12T10:00:01.000Z".into(),
                }],
            }
        )
    );
    assert_json_snapshot!(
        "session_delete",
        (
            SessionDeleteParams {
                id: "s1".into(),
                purge_traces: true,
            },
            SessionDeleteResult {
                messages_deleted: 7,
                events_deleted: 41,
            }
        )
    );
    assert_json_snapshot!(
        "session_export",
        SessionExport {
            format: SESSION_EXPORT_FORMAT.into(),
            exported_at: "2026-09-12T11:00:00.000Z".into(),
            session: info,
            messages,
            mentor_calls: vec![MentorCallInfo {
                id: "c1".into(),
                agent_id: "a1".into(),
                step_id: "st9".into(),
                kind: "title".into(),
                model: "claude-haiku-4-5-20251001".into(),
                effort: Some("low".into()),
                started_at: "2026-09-12T10:04:31.000Z".into(),
                ended_at: Some("2026-09-12T10:04:32.000Z".into()),
                status: "ok".into(),
                stop_reason: Some("end_turn".into()),
                usage: Some(Usage {
                    input_tokens: 80,
                    output_tokens: 7,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                }),
                cost_usd: Some(0.000_115),
                first_byte_ms: Some(300),
                total_ms: Some(410),
            }],
        }
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
fn bundle_shapes() {
    let counts = BundleCounts {
        sessions: 2,
        workspaces: 1,
        agents: 3,
        steps: 9,
        events: 120,
        mentor_calls: 9,
        messages: 24,
        blobs: 40,
        blob_bytes: 1_048_576,
    };
    let manifest = BundleManifest {
        format_version: BUNDLE_FORMAT_VERSION,
        created_at: "2026-09-13T10:00:00.000Z".into(),
        harness_version: "0.1.0".into(),
        schema_version: 3,
        selection: BundleSelection {
            session_ids: vec![],
            workspace_id: Some("w1".into()),
            since: Some("2026-09-06T10:00:00.000Z".into()),
            until: None,
            all: false,
        },
        sessions: vec![BundleSession {
            id: "s1".into(),
            title: Some("fix tests".into()),
            workspace_id: Some("w1".into()),
            created_at: "2026-09-12T10:00:00.000Z".into(),
            updated_at: "2026-09-12T10:30:00.000Z".into(),
            status: "open".into(),
            agents: 2,
            events: 80,
            mentor_calls: 6,
            messages: 16,
        }],
        counts,
        redaction: Some(RedactionReport {
            applied: true,
            rules: vec![
                RedactionRule {
                    name: "anthropic_key".into(),
                    source: "builtin".into(),
                    matches: 4,
                },
                RedactionRule {
                    name: "ticket".into(),
                    source: "user".into(),
                    matches: 1,
                },
                RedactionRule {
                    name: "paths".into(),
                    source: "paths".into(),
                    matches: 12,
                },
            ],
            replacements: 17,
            secrets: 2,
            paths: true,
            blob_map: [("a".repeat(64), "b".repeat(64))].into_iter().collect(),
            touched_requests: 6,
            replayable: false,
        }),
    };
    assert_json_snapshot!(
        "trace_export",
        (
            TraceExportParams {
                output: "C:/exports/week.tar.zst".into(),
                session_ids: vec![],
                workspace_id: Some("w1".into()),
                since: Some("7d".into()),
                until: None,
                all: false,
                redact: true,
                redact_paths: true,
            },
            TraceExportResult {
                path: "C:/exports/week.tar.zst".into(),
                manifest,
            }
        )
    );
    assert_json_snapshot!(
        "trace_import",
        (
            TraceImportParams {
                path: "C:/exports/week.tar.zst".into(),
                into_workspace: Some("w2".into()),
                keep_ids: false,
            },
            TraceImportResult {
                sessions: vec![ImportedSession {
                    from: "s1".into(),
                    to: "s9".into(),
                }],
                counts,
                redacted: true,
                blobs_written: 38,
            }
        )
    );
    assert_json_snapshot!(
        "trace_replay_check",
        (
            TraceReplayCheckParams {
                session_id: Some("s1".into()),
                agent_id: None,
                call_id: None,
                since: None,
                until: None,
                rebuild: true,
            },
            ReplayReport {
                calls: vec![
                    ReplayCall {
                        call_id: "m1".into(),
                        session_id: "s1".into(),
                        agent_id: "a1".into(),
                        step_id: "st1".into(),
                        kind: "step".into(),
                        request_event_id: "e7".into(),
                        status: ReplayStatus::Ok,
                        checks: ["blob", "hash", "parse", "canonical", "rebuild"]
                            .map(String::from)
                            .to_vec(),
                        problems: vec![],
                    },
                    ReplayCall {
                        call_id: "m2".into(),
                        session_id: "s1".into(),
                        agent_id: "a1".into(),
                        step_id: "st2".into(),
                        kind: "step".into(),
                        request_event_id: "e12".into(),
                        status: ReplayStatus::Failed,
                        checks: ["blob", "hash"].map(String::from).to_vec(),
                        problems: vec!["body hashes to bbbb, not its id aaaa".into()],
                    },
                    ReplayCall {
                        call_id: "m3".into(),
                        session_id: "s1".into(),
                        agent_id: "a1".into(),
                        step_id: "st3".into(),
                        kind: "title".into(),
                        request_event_id: "e20".into(),
                        status: ReplayStatus::Skipped,
                        checks: ["blob", "hash", "parse", "canonical", "rebuild"]
                            .map(String::from)
                            .to_vec(),
                        problems: vec![
                            "rebuild skipped: a `title` call is not built from the conversation"
                                .into(),
                        ],
                    },
                ],
                checked: 3,
                passed: 1,
                failed: 1,
                skipped: 1,
                rebuild: true,
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
        "workspace_init",
        (
            WorkspaceInitParams {
                id: "w1".into(),
                force: false,
            },
            WorkspaceInitResult {
                path: "/src/repo/.harness/HARNESS.md".into(),
                replaced: false,
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
            git_dirty: Some(true),
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
            event_id: Some("ev12".into()),
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
            session_calls: 2,
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
        Event::AgentOutcome {
            agent_id: "a1".into(),
            event_id: "e20".into(),
            kind: "tests".into(),
            summary: "11 passed, 1 failed (cargo test)".into(),
            ok: Some(false),
            details: json!({
                "runner": "cargo",
                "passed": 11,
                "failed": 1,
                "skipped": 0,
                "exit_code": 101,
                "duration_ms": 2410,
                "command": "cargo test",
                "source": "shell",
            }),
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
