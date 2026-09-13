// Drift check: the Rust snapshots in `crates/api/tests/snapshots` (task
// M00-02) must parse against the hand-written types in `api.ts`.

import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  ALL_METHODS,
  API_VERSION,
  type AgentRunResult,
  type EventNotification,
  type HelloResult,
  KNOWN_EVENT_TYPES,
  type OutcomeStats,
  type PromptShowParams,
  type PromptShowResult,
  type ReplayReport,
  SESSION_EXPORT_FORMAT,
  type SessionExport,
  type SessionGetParams,
  type SessionGetResult,
  type SessionListParams,
  type SessionListResult,
  type SessionMarkParams,
  type SessionMarkResult,
  type SessionSearchParams,
  type SessionSearchResult,
  type StatsCallsParams,
  type StatsCallsResult,
  type StatsOutcomesParams,
  type StatsTokensParams,
  type TokenStats,
  type ToolsListResult,
  type TraceExportParams,
  type TraceExportResult,
  type TraceImportParams,
  type TraceImportResult,
  type TraceReplayCheckParams,
  type WorkspaceAddParams,
  type WorkspaceInfoResult,
  type WorkspaceInitParams,
  type WorkspaceInitResult,
  type WorkspaceSummary,
  asKnown,
  eventShapeError,
  isEventNotification,
  isRpcError,
} from "./api";

const SNAPSHOTS = join(
  dirname(fileURLToPath(import.meta.url)),
  "../../../../crates/api/tests/snapshots",
);

/** The JSON body of an insta `.snap` file (after the `---` header). */
function snapshot(name: string): unknown {
  const text = readFileSync(join(SNAPSHOTS, `snapshots__${name}.snap`), "utf8").replace(
    /\r\n/g,
    "\n",
  );
  const body = text.split("\n---\n").slice(1).join("\n---\n");
  return JSON.parse(body);
}

describe("api.ts against the Rust snapshots", () => {
  it("finds the snapshot directory", () => {
    const files = readdirSync(SNAPSHOTS).filter((f) => f.endsWith(".snap"));
    expect(files.length).toBeGreaterThan(10);
  });

  it("parses every event the daemon can emit", () => {
    const notifications = snapshot("events") as unknown[];
    expect(notifications.length).toBeGreaterThanOrEqual(KNOWN_EVENT_TYPES.length);
    const seen = new Set<string>();
    for (const n of notifications) {
      expect(isEventNotification(n), JSON.stringify(n)).toBe(true);
      const ev = (n as EventNotification).event;
      expect(eventShapeError(ev), JSON.stringify(ev)).toBeUndefined();
      seen.add(ev.type);
    }
    // Every type this build knows appears in the snapshot, so a Rust-side
    // rename shows up as a missing type here.
    for (const t of KNOWN_EVENT_TYPES) expect(seen.has(t), t).toBe(true);
  });

  it("reads usage, cost and the terminal status from the events", () => {
    const notifications = snapshot("events") as EventNotification[];
    const known = notifications.map((n) => asKnown(n.event));
    const usage = known.find((e) => e?.type === "agent.usage");
    expect(usage).toMatchObject({
      usage: { input_tokens: 1204, output_tokens: 310 },
      cost_usd: 0.0138,
    });
    const finished = known.find((e) => e?.type === "agent.finished");
    expect(finished).toMatchObject({ status: "error" });
    if (finished?.type === "agent.finished") {
      expect(isRpcError(finished.error)).toBe(true);
      expect(finished.error?.data?.kind).toBe("cancelled");
    }
  });

  it("matches the hello, error, agent.run and stats shapes", () => {
    const hello = snapshot("hello_response") as { result: HelloResult };
    expect(hello.result.api_version).toBe(API_VERSION);
    expect(typeof hello.result.daemon_version).toBe("string");
    expect(typeof hello.result.pid).toBe("number");

    const err = snapshot("error_response") as { error: unknown };
    expect(isRpcError(err.error)).toBe(true);

    const run = snapshot("agent_run_result") as AgentRunResult;
    expect(typeof run.agent_id).toBe("string");
    expect(typeof run.subscription).toBe("string");

    const stats = snapshot("token_stats") as TokenStats;
    expect(typeof stats.totals.cost_usd).toBe("number");
    expect(Array.isArray(stats.by_model)).toBe(true);
    expect(typeof stats.apprentice.invocations).toBe("number");
  });

  it("reads the tool list", () => {
    const [, result] = snapshot("tools_list") as [unknown, ToolsListResult];
    expect(result.tools.map((t) => [t.name, t.risk, t.enabled])).toEqual([
      ["read_file", "read_only", true],
      ["shell", "execute", false],
    ]);
  });

  it("reads the assembled prompt", () => {
    const [params, result] = snapshot("prompt_show") as [PromptShowParams, PromptShowResult];
    expect(params.count).toBe(true);
    expect(result.version).toBe("mentor_system_v1");
    expect(result.blocks.map((b) => b.cache)).toEqual([true, true]);
    expect(result.blocks[1]?.text.startsWith("#workspace\n")).toBe(true);
    expect(result.tokens).toBe(1042);
  });

  it("reads the agents of a session and the trace event of a tool result", () => {
    const [params, result] = snapshot("session_get") as [SessionGetParams, SessionGetResult];
    expect(params.before_seq).toBe(3);
    expect(result.agents?.map((a) => [a.id, a.status, a.model, a.calls])).toEqual([
      ["a1", "ok", "claude-opus-5", 2],
    ]);
    const notifications = snapshot("events") as EventNotification[];
    const result_ev = notifications.map((n) => n.event).find((e) => e.type === "agent.tool_result");
    expect(result_ev).toBeDefined();
    if (result_ev?.type === "agent.tool_result") expect(result_ev.event_id).toBe("ev12");
  });

  it("reads sessions, their messages, search hits and an export", () => {
    const [listParams, list] = snapshot("session_list") as [SessionListParams, SessionListResult];
    expect(listParams.query).toBe("hello");
    expect(list.sessions[0]?.status).toBe("open");
    expect(list.sessions[0]?.message_count).toBe(7);
    expect(list.sessions[0]?.last_agent_status).toBe("ok");
    expect(list.sessions[0]?.running_agent).toBe("a2");
    expect(list.sessions[0]?.cost_usd).toBe(0.0138);
    expect(list.sessions[0]?.calls).toBe(2);
    const [, got] = snapshot("session_get") as [SessionGetParams, SessionGetResult];
    expect(got.session.prompt_version).toBe("mentor_system_v1");
    expect(got.messages.map((m) => m.role)).toEqual(["user", "assistant"]);
    expect(got.has_more).toBe(true);
    const [, search] = snapshot("session_search") as [SessionSearchParams, SessionSearchResult];
    expect(search.hits[0]?.snippet).toContain("[hello]");
    const exported = snapshot("session_export") as SessionExport;
    expect(exported.format).toBe(SESSION_EXPORT_FORMAT);
    expect(exported.mentor_calls[0]?.kind).toBe("title");
  });

  it("reads the bundle shapes", () => {
    const [params, exported] = snapshot("trace_export") as [TraceExportParams, TraceExportResult];
    expect(params.redact_paths).toBe(true);
    expect(exported.manifest.format_version).toBe(1);
    expect(exported.manifest.counts.blobs).toBe(40);
    expect(exported.manifest.redaction?.rules.map((r) => r.source)).toEqual([
      "builtin",
      "user",
      "paths",
    ]);
    expect(exported.manifest.redaction?.replayable).toBe(false);
    const [imp, imported] = snapshot("trace_import") as [TraceImportParams, TraceImportResult];
    expect(imp.into_workspace).toBe("w2");
    expect(imported.sessions[0]).toEqual({ from: "s1", to: "s9" });
    const [check, report] = snapshot("trace_replay_check") as [
      TraceReplayCheckParams,
      ReplayReport,
    ];
    expect(check.rebuild).toBe(true);
    expect(report.calls.map((c) => c.status)).toEqual(["ok", "failed", "skipped"]);
    expect(report.calls[1]?.problems?.[0]).toContain("hashes to");
  });

  it("reads outcomes: a run's signals, a mark, the stats", () => {
    const [, page] = snapshot("session_get") as [SessionGetParams, SessionGetResult];
    const agent = page.agents?.[0];
    expect(agent?.outcomes?.map((o) => [o.kind, o.ok])).toEqual([
      ["tests", true],
      ["files_changed", undefined],
    ]);
    expect(agent?.outcomes?.[0]?.summary).toBe("12 passed (cargo test)");
    expect(agent?.error).toBeUndefined();
    const [params, marked] = snapshot("session_mark") as [SessionMarkParams, SessionMarkResult];
    expect(params.mark).toBe("accept");
    expect(marked.outcome.kind).toBe("user_accept");
    expect(marked.outcome.ok).toBe(true);
    const [range, stats] = snapshot("stats_outcomes") as [StatsOutcomesParams, OutcomeStats];
    expect(range.since).toBe("7d");
    expect(stats.labelled_share).toBe(0.65);
    expect(stats.by_kind?.tests).toBe(11);
    expect(stats.errors?.stalled).toBe(1);
    const [init, written] = snapshot("workspace_init") as [
      WorkspaceInitParams,
      WorkspaceInitResult,
    ];
    expect(init.force).toBeUndefined();
    expect(written.path.endsWith("HARNESS.md")).toBe(true);
    const events = snapshot("events") as EventNotification[];
    const outcome = events.map((n) => asKnown(n.event)).find((e) => e?.type === "agent.outcome");
    expect(outcome?.type === "agent.outcome" && outcome.ok).toBe(false);
  });

  it("reads workspace info", () => {
    const info = snapshot("workspace_info") as WorkspaceInfoResult;
    expect(info.file_count).toBe(1234);
    expect(info.git_branch).toBe("main");
    expect(info.git_dirty).toBe(true);
    expect(info.index_truncated).toBeUndefined();
    expect(info.config_overrides).toEqual(["mentor.effort"]);
    const [params, added] = snapshot("workspace_add") as [WorkspaceAddParams, WorkspaceSummary];
    expect(params.root).toBe("C:/src/repo");
    expect(added.id).toBe("w1");
  });

  it("reads token stats with the new breakdowns and a page of calls", () => {
    const stats = snapshot("token_stats") as TokenStats;
    expect(stats.by_kind[0]?.key).toBe("step");
    expect(stats.by_workspace).toEqual([]);
    const grouped = snapshot("stats_tokens_params_grouped") as StatsTokensParams;
    expect(grouped.group_by).toEqual(["day", "workspace", "kind"]);
    const [params, page] = snapshot("stats_calls") as [StatsCallsParams, StatsCallsResult];
    expect(params.offset).toBe(100);
    expect(page.total).toBe(102);
    expect(page.calls.map((c) => [c.kind, c.status, c.cost_usd])).toEqual([
      ["title", "ok", 0.00011],
      ["step", "error", undefined],
    ]);
    expect(page.calls[0]?.request_event_id).toBe("e9");
  });

  it("lists every method the daemon knows, namespaced", () => {
    expect(ALL_METHODS.length).toBe(42);
    for (const m of ALL_METHODS) expect(m).toMatch(/^[a-z]+\.[a-z_]+$/);
  });

  it("skips unknown event types instead of failing", () => {
    const n: unknown = { subscription: "a", seq: 1, event: { type: "agent.future_thing", x: 1 } };
    expect(isEventNotification(n)).toBe(true);
    expect(asKnown((n as EventNotification).event)).toBeUndefined();
    expect(eventShapeError((n as EventNotification).event)).toBeUndefined();
  });
});
