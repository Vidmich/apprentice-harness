// A scripted daemon for running the page outside Tauri (`pnpm dev` in a
// browser): enough of the RPC surface for the chat view — sessions with
// stored rows and agents, `agent.run` streaming a canned answer with
// thinking, markdown, a read, an edit (with a diff) and a shell call
// with streamed output, `agent.cancel`, `agent.subscribe`, and the trace
// events behind the Raw tab; workspaces, the sessions list and search,
// rename/archive/delete/export, the config and the permission rules
// (task M01-12); the mentor calls behind `stats.tokens` / `stats.calls`
// with their request bodies in the trace, and ten days of seeded
// history for the usage panel (task M01-13). Words in the prompt steer
// the script: "fail" ends the
// run with an error before any tool, "slow" makes the shell run for a
// minute (try Cancel), "big" gives it 5 MB of output, "ask" makes the
// edit ask for permission (unless a rule in the mock's files allows or
// denies it). The state lives in `sessionStorage` (blobs aside), so a
// reload finds its sessions again and a run in progress carries on —
// the reattach path. Nothing here is used in the app: `bridge.ts` loads
// it only when `isTauri()` is false.

import type {
  AgentSummary,
  CallSummary,
  EventNotification,
  EventSummary,
  RpcError,
  RuleInfo,
  RuleSpec,
  SessionGetResult,
  SessionInfo,
  SessionMessage,
  SessionSearchHit,
  StatsGroup,
  TokenBucket,
  TraceEvent,
  Usage,
  WorkspaceSummary,
} from "./api";
import type { Backend, EventHandler } from "./bridge";

interface Session {
  info: SessionInfo;
  messages: SessionMessage[];
  agents: AgentSummary[];
}

/** A pending `permission.request` and the promise its answer settles. */
interface Ask {
  sessionId: string;
  agentId: string;
  resolve: (answer: string) => void;
}

/** The two rules files, as `[[rule]]` arrays. */
interface RuleFiles {
  user: RuleSpec[];
  workspace: Record<string, RuleSpec[]>;
}

interface Run {
  agentId: string;
  sessionId: string;
  prompt: string;
  channels: Set<string>;
  seq: number;
  cancelled: boolean;
  finished?: EventNotification;
}

const STEP_MS = 16;
const STORE_KEY = "harness.mock";
/** Blobs above this are not kept across a reload. */
const KEEP_BLOB_BYTES = 200_000;

interface Persisted {
  ids: number;
  sessions: Session[];
  runs: { agentId: string; sessionId: string; prompt: string }[];
  trace: { event: TraceEvent; blob?: string }[];
  calls?: CallSummary[];
  workspaces?: WorkspaceSummary[];
  config?: Record<string, unknown>;
  workspaceConfig?: Record<string, Record<string, unknown>>;
  rules?: RuleFiles;
  keyConfigured?: boolean;
}

/** The daemon's defaults for the keys the settings screen shows. */
const DEFAULTS: Record<string, unknown> = {
  "mentor.model": "claude-opus-5",
  "mentor.effort": "high",
  "mentor.thinking_display": "summarized",
  "mentor.max_tokens": 64000,
  "permissions.default_mode": "default",
  "permissions.headless": "deny",
  "permissions.ask_timeout_s": 600,
  "tools.shell.program": "",
  "tools.shell.args": [],
  "tools.shell.max_timeout_s": 3600,
  "sessions.auto_title": true,
};

const WORKSPACE_KEYS = /^(mentor\.(model|effort|max_tokens)|permissions\.|tools\.|runtime\.)/;

/** The mock's price table: USD per token, in and out. */
const PRICES: Record<string, [number, number]> = {
  "claude-opus-5": [0.000005, 0.000025],
  "claude-sonnet-5": [0.000003, 0.000015],
  "claude-haiku-4-5-20251001": [0.000001, 0.000005],
};
const TITLE_MODEL = "claude-haiku-4-5-20251001";

function priceOf(model: string, input: number, output: number): number | undefined {
  const p = PRICES[model];
  return p === undefined ? undefined : input * p[0] + output * p[1];
}

const BUILTIN_RULES: RuleInfo[] = [
  {
    source: "builtin",
    index: 1,
    name: "read_only_inside",
    rule: { tool: "*", effect: "allow", match: { risk: "read_only", outside_workspace: false } },
  },
  {
    source: "builtin",
    index: 2,
    name: "deny_git_push",
    rule: { tool: "shell", effect: "deny", match: { command_regex: "git\\s+push" } },
  },
];

const MOCK_TOOLS = [
  "read_file",
  "write_file",
  "edit_file",
  "glob",
  "grep",
  "list_files",
  "shell",
  "git_status",
  "git_diff",
];

function now(): string {
  return new Date().toISOString();
}

function rpcError(code: number, kind: string, message: string): RpcError {
  return { code, message, data: { kind } };
}

const ANSWER = `Here is what I found.

The module reads its config **twice**; the second read wins. I'd change it like this:

\`\`\`rust
fn load(path: &Path) -> Result<Config, Error> {
    let text = fs::read_to_string(path)?;
    toml::from_str(&text).map_err(Error::from)
}
\`\`\`

Then the call sites:

1. \`main.rs\` — drop the early read
2. \`daemon.rs\` — pass the loaded value

| file | change |
| --- | --- |
| main.rs | −4 lines |
| daemon.rs | +2 lines |

See the [Tauri docs](https://v2.tauri.app/) for the window API. Done — the tests pass.`;

const EDIT_RESULT = `edited src/main.rs (+2 -1, 1 replacement)

--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,5 @@
 fn main() {
-    let cfg = load_twice();
+    let cfg = load(Path::new("harness.toml"))?;
+    run(cfg);
     println!("ready");
 }
`;

/** The scripted backend. */
export function mockBackend(): Backend {
  const sessions = new Map<string, Session>();
  const runs = new Map<string, Run>();
  const trace = new Map<string, { event: TraceEvent; blob?: string }>();
  const listeners = new Map<string, Set<EventHandler<unknown>>>();
  const workspaces = new Map<string, WorkspaceSummary>();
  /** Every mentor call, oldest first. */
  const calls: CallSummary[] = [];
  const asks = new Map<string, Ask>();
  let config: Record<string, unknown> = {};
  let workspaceConfig: Record<string, Record<string, unknown>> = {};
  let rules: RuleFiles = { user: [], workspace: {} };
  let keyConfigured = true;
  let ids = 0;
  const nextId = (prefix: string) => `${prefix}_${(++ids).toString(36).padStart(4, "0")}`;

  function save() {
    try {
      const state: Persisted = {
        ids,
        sessions: [...sessions.values()],
        runs: [...runs.values()].map(({ agentId, sessionId, prompt }) => ({
          agentId,
          sessionId,
          prompt,
        })),
        trace: [...trace.values()].map((e) =>
          e.blob !== undefined && e.blob.length <= KEEP_BLOB_BYTES ? e : { event: e.event },
        ),
        calls,
        workspaces: [...workspaces.values()],
        config,
        workspaceConfig,
        rules,
        keyConfigured,
      };
      sessionStorage.setItem(STORE_KEY, JSON.stringify(state));
    } catch {
      // Too large or unavailable: the next reload starts empty.
    }
  }

  const emit = (event: string, payload: unknown) => {
    for (const h of listeners.get(event) ?? []) h(payload);
  };

  const zero: Usage = {
    input_tokens: 0,
    output_tokens: 0,
    cache_read_input_tokens: 0,
    cache_creation_input_tokens: 0,
  };

  /** Registers a root (idempotent) and marks it used. */
  function addWorkspace(root: string, name?: string): WorkspaceSummary {
    const canonical = root.replace(/[\\/]+$/, "");
    const existing = [...workspaces.values()].find((w) => w.root === canonical);
    const ts = now();
    if (existing !== undefined) {
      existing.last_used_at = ts;
      save();
      return existing;
    }
    const w: WorkspaceSummary = {
      id: nextId("w"),
      root: canonical,
      name: name ?? canonical.split(/[\\/]/).filter(Boolean).pop() ?? canonical,
      created_at: ts,
      last_used_at: ts,
    };
    workspaces.set(w.id, w);
    save();
    return w;
  }

  function createSession(workspace: string | undefined): Session {
    const id = nextId("s");
    const ts = now();
    const ws = workspace === undefined ? undefined : addWorkspace(workspace);
    const info: SessionInfo = {
      id,
      workspace: ws?.root ?? null,
      workspace_id: ws?.id ?? null,
      status: "open",
      created_at: ts,
      updated_at: ts,
      message_count: 0,
      last_activity: ts,
      usage: zero,
      calls: 0,
      config: {},
    };
    const s = { info, messages: [], agents: [] };
    sessions.set(id, s);
    return s;
  }

  /** Totals over some calls, the daemon's way (a cost only when every call has one). */
  function totalsOf(rows: CallSummary[]): {
    usage: Usage;
    cost?: number;
    calls: number;
    unpriced: number;
  } {
    const usage = { ...zero };
    let cost = 0;
    let unpriced = 0;
    for (const c of rows) {
      usage.input_tokens += c.input;
      usage.output_tokens += c.output;
      usage.cache_read_input_tokens += c.cache_read;
      usage.cache_creation_input_tokens += c.cache_creation;
      if (c.cost_usd === undefined) {
        if (c.status === "ok") unpriced += 1;
      } else cost += c.cost_usd;
    }
    const out: ReturnType<typeof totalsOf> = { usage, calls: rows.length, unpriced };
    if (unpriced === 0) out.cost = cost;
    return out;
  }

  /**
   * Records a completed mentor call: its `mentor.request` event with the
   * body as the blob, and the row `stats.calls` lists.
   */
  function recordCall(
    s: Session,
    agentId: string,
    kind: "step" | "title",
    model: string,
    input: number,
    output: number,
    over: Partial<CallSummary> = {},
  ): CallSummary {
    const startedAt = over.started_at ?? now();
    const body = JSON.stringify({
      model,
      max_tokens: kind === "title" ? 32 : 64000,
      system: kind === "title" ? "Name this conversation in a few words." : "(the system prompt)",
      messages: s.messages.slice(-4).map((m) => ({ role: m.role, content: m.content })),
      thinking: kind === "title" ? undefined : { type: "adaptive" },
    });
    const eventId = record(s, agentId, "mentor.request", { model, kind, bytes: body.length }, body);
    const c: CallSummary = {
      call_id: nextId("m"),
      started_at: startedAt,
      session_id: s.info.id,
      agent_id: agentId,
      kind,
      model,
      status: "ok",
      stop_reason: "end_turn",
      input,
      output,
      cache_read: 0,
      cache_creation: 0,
      total_ms: 400 + Math.round(output * 12),
      request_event_id: eventId,
      ...over,
    };
    if (s.info.title !== undefined && s.info.title !== null) c.session_title = s.info.title;
    if (s.info.workspace_id !== undefined && s.info.workspace_id !== null) {
      c.workspace_id = s.info.workspace_id;
    }
    if (kind === "step") c.effort = "high";
    if (c.status !== "ok") delete c.stop_reason;
    const cost = c.status === "ok" ? priceOf(model, input, output) : undefined;
    if (cost !== undefined) c.cost_usd = cost;
    calls.push(c);
    save();
    return c;
  }

  /** The list row of a session, with the totals and the running agent. */
  function summary(s: Session): SessionInfo {
    const mine = calls.filter((c) => c.session_id === s.info.id);
    const t = totalsOf(mine);
    const last = s.agents[s.agents.length - 1];
    const out: SessionInfo = { ...s.info, usage: t.usage, calls: t.calls };
    if (t.cost !== undefined && t.calls > 0) out.cost_usd = t.cost;
    if (last !== undefined && last.status !== "running") {
      out.last_agent_status = last.status as "ok" | "cancelled" | "error";
    }
    const running = [...runs.values()].find((r) => r.sessionId === s.info.id);
    if (running !== undefined) out.running_agent = running.agentId;
    return out;
  }

  /** The text of every block of a message, for the search. */
  function textOf(m: SessionMessage): string {
    return m.content
      .map((block) => {
        const b = block as { type?: string; text?: string; content?: unknown };
        if (b.type === "text" && typeof b.text === "string") return b.text;
        if (b.type === "tool_result" && Array.isArray(b.content)) {
          return b.content.map((c) => (c as { text?: string }).text ?? "").join(" ");
        }
        return "";
      })
      .join(" ");
  }

  /** The rules in force for a session, most specific file first. */
  function rulesFor(workspaceRoot: string | null | undefined): RuleSpec[] {
    const ws = workspaceRoot == null ? [] : (rules.workspace[workspaceRoot] ?? []);
    return [...ws, ...rules.user];
  }

  /** The first rule of the files that names the tool and matches the command. */
  function ruleDecides(
    workspaceRoot: string | null | undefined,
    tool: string,
    command: string | undefined,
    path: string | undefined,
  ): "allow" | "deny" | undefined {
    for (const r of rulesFor(workspaceRoot)) {
      if (r.tool !== "*" && r.tool !== tool) continue;
      const m = r.match ?? {};
      if (m.command_prefix !== undefined) {
        if (command === undefined) continue;
        const c = command.trim();
        if (!(c === m.command_prefix || c.startsWith(`${m.command_prefix} `))) continue;
      }
      if (m.command_regex !== undefined) {
        if (command === undefined || !new RegExp(m.command_regex).test(command)) continue;
      }
      if (m.path !== undefined) {
        if (path === undefined) continue;
        // `**` is anything, `*` anything but a separator.
        const glob = m.path
          .replace(/[.+^${}()|[\]]/g, "\\$&")
          .split("**")
          .map((part) => part.replace(/\*/g, "[^/]*"))
          .join(".*");
        if (!new RegExp(`^${glob}$`).test(path)) continue;
      }
      if (r.effect === "allow" || r.effect === "deny") return r.effect;
    }
    return undefined;
  }

  /**
   * Asks the run's subscribers; resolves with the answer, `deny_once`
   * after the timeout or when nobody listens.
   */
  function askPermission(
    s: Session,
    run: Run,
    tool: string,
    input: Record<string, unknown>,
    suggested: RuleSpec[],
  ): Promise<{ requestId?: string; answer: string }> {
    if (run.channels.size === 0) return Promise.resolve({ answer: "headless" });
    const requestId = nextId("p");
    const timeoutS = Number(
      config["permissions.ask_timeout_s"] ?? DEFAULTS["permissions.ask_timeout_s"],
    );
    return new Promise((resolve) => {
      const timer = setTimeout(() => {
        asks.delete(requestId);
        resolve({ requestId, answer: "timeout" });
      }, timeoutS * 1000);
      asks.set(requestId, {
        sessionId: s.info.id,
        agentId: run.agentId,
        resolve: (answer) => {
          clearTimeout(timer);
          asks.delete(requestId);
          resolve({ requestId, answer });
        },
      });
      const ev: Record<string, unknown> = {
        type: "permission.request",
        request_id: requestId,
        agent_id: run.agentId,
        tool,
        input,
        risk: tool === "shell" ? "execute" : "write",
        description: `${tool}: ${String(input.command ?? input.path ?? "")}`,
        suggested_rules: suggested,
        timeout_s: timeoutS,
      };
      if (typeof input.command === "string") ev.command = input.command;
      if (typeof input.path === "string") ev.paths = [input.path];
      send(run, ev);
    });
  }

  function push(
    s: Session,
    role: "user" | "assistant",
    content: unknown[],
    agent: string,
    step?: string,
  ) {
    const last = s.messages[s.messages.length - 1];
    if (role === "user" && last?.role === "user" && step === undefined) {
      last.content = [...last.content, ...content];
      last.agent_id = agent;
      return;
    }
    const m: SessionMessage = {
      seq: s.messages.length + 1,
      role,
      content,
      agent_id: agent,
      created_at: now(),
    };
    if (step !== undefined) m.step_id = step;
    s.messages.push(m);
    s.info.message_count = s.messages.length;
    s.info.last_activity = m.created_at;
    save();
  }

  function record(
    s: Session,
    agent: string,
    kind: string,
    payload: unknown,
    blob?: string,
  ): string {
    const id = nextId("ev");
    const event: TraceEvent = {
      id,
      session_id: s.info.id,
      agent_id: agent,
      seq: trace.size + 1,
      ts: now(),
      kind,
      payload,
      blob_id: blob === undefined ? null : `b_${id}`,
      blob_bytes: blob?.length ?? null,
    };
    const entry: { event: TraceEvent; blob?: string } = { event };
    if (blob !== undefined) entry.blob = blob;
    trace.set(id, entry);
    save();
    return id;
  }

  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

  function send(run: Run, event: Record<string, unknown>) {
    const n: EventNotification = {
      subscription: run.agentId,
      seq: ++run.seq,
      event: event as never,
    };
    for (const ch of run.channels) emit(`rpc:event:${ch}`, n);
    if (event.type === "agent.finished") run.finished = n;
  }

  // A hidden tab gets one timer a second: bigger chunks keep a run short.
  const chunk = () => (document.hidden ? 80 : 3);

  async function stream(run: Run, type: "agent.text_delta" | "agent.thinking_delta", text: string) {
    for (let i = 0; i < text.length && !run.cancelled;) {
      const n = chunk();
      send(run, { type, agent_id: run.agentId, text: text.slice(i, i + n) });
      i += n;
      await sleep(STEP_MS);
    }
  }

  /**
   * The canned run. `done` assistant rows of the agent exist already
   * (a run resumed after a reload): those steps are skipped.
   */
  async function runScript(s: Session, run: Run, prompt: string) {
    const a = run.agentId;
    const agent = s.agents.find((x) => x.id === a)!;
    const turn: AgentSummary = agent;
    const done = s.messages.filter((m) => m.role === "assistant" && m.agent_id === a).length;
    const finish = (status: "ok" | "cancelled" | "error", error?: RpcError) => {
      turn.status = status;
      turn.ended_at = now();
      const ev: Record<string, unknown> = { type: "agent.finished", agent_id: a, status };
      if (error !== undefined) ev.error = error;
      send(run, ev);
      runs.delete(a);
      save();
    };
    const usage = (input: number, output: number) => {
      turn.calls += 1;
      turn.usage = {
        ...turn.usage,
        input_tokens: turn.usage.input_tokens + input,
        output_tokens: turn.usage.output_tokens + output,
      };
      const c = recordCall(s, a, "step", "claude-opus-5", input, output);
      turn.cost_usd = (turn.cost_usd ?? 0) + (c.cost_usd ?? 0);
      const t = totalsOf(calls.filter((x) => x.session_id === s.info.id));
      const ev: Record<string, unknown> = {
        type: "agent.usage",
        agent_id: a,
        call_id: c.call_id,
        usage: { ...zero, input_tokens: input, output_tokens: output },
        cost_usd: c.cost_usd,
        session_usage: t.usage,
        session_calls: t.calls,
      };
      if (t.cost !== undefined) ev.session_cost_usd = t.cost;
      send(run, ev);
    };

    if (done === 0) send(run, { type: "agent.started", agent_id: a, session_id: s.info.id });
    await sleep(300);
    if (prompt.includes("fail")) {
      send(run, { type: "agent.step", agent_id: a, seq: 1, phase: "mentor" });
      await sleep(400);
      finish("error", rpcError(-32000, "mentor_overloaded", "the mentor is overloaded; try again"));
      return;
    }
    turn.model = "claude-opus-5";

    // Step 1: think, say a word, read a file.
    if (done < 1) {
      send(run, { type: "agent.step", agent_id: a, seq: 1, phase: "mentor" });
      const thinking =
        "The user wants the config loaded once. Let me look at main.rs first, then edit it.";
      await stream(run, "agent.thinking_delta", thinking);
      await stream(run, "agent.text_delta", "Let me read the file first.");
      const c1 = nextId("toolu");
      send(run, {
        type: "agent.tool_call",
        agent_id: a,
        call_id: c1,
        name: "read_file",
        input: { path: "src/main.rs" },
      });
      usage(1200, 60);
      push(
        s,
        "assistant",
        [
          { type: "thinking", thinking, signature: "sig" },
          { type: "text", text: "Let me read the file first." },
          { type: "tool_use", id: c1, name: "read_file", input: { path: "src/main.rs" } },
        ],
        a,
        "st1",
      );
      send(run, { type: "agent.step", agent_id: a, seq: 1, phase: "tools" });
      await sleep(300);
      const fileText = Array.from(
        { length: 40 },
        (_, i) => `${i + 1}: line ${i + 1} of main.rs`,
      ).join("\n");
      const ev1 = record(
        s,
        a,
        "tool.result",
        {
          call_id: c1,
          name: "read_file",
          ok: true,
          output_bytes: fileText.length,
          media_type: "text/plain",
        },
        fileText,
      );
      send(run, {
        type: "agent.tool_result",
        agent_id: a,
        call_id: c1,
        name: "read_file",
        ok: true,
        summary: "read 40 lines",
        blob_id: `b_${ev1}`,
        mentor_bytes: fileText.length,
        event_id: ev1,
      });
      push(
        s,
        "user",
        [{ type: "tool_result", tool_use_id: c1, content: [{ type: "text", text: fileText }] }],
        a,
        "st1",
      );
      if (run.cancelled) return finish("cancelled");
    }

    // Step 2: edit it and run the tests.
    if (done < 2) {
      send(run, { type: "agent.step", agent_id: a, seq: 2, phase: "mentor" });
      await stream(run, "agent.text_delta", "Now the edit, then the tests.");
      const c2 = nextId("toolu");
      const c3 = nextId("toolu");
      const edit = {
        path: "src/main.rs",
        old_string: "    let cfg = load_twice();",
        new_string: '    let cfg = load(Path::new("harness.toml"))?;\n    run(cfg);',
      };
      const shell = { command: prompt.includes("slow") ? "cargo test -- --ignored" : "cargo test" };
      send(run, {
        type: "agent.tool_call",
        agent_id: a,
        call_id: c2,
        name: "edit_file",
        input: edit,
      });
      send(run, { type: "agent.tool_call", agent_id: a, call_id: c3, name: "shell", input: shell });
      usage(1800, 120);
      push(
        s,
        "assistant",
        [
          { type: "text", text: "Now the edit, then the tests." },
          { type: "tool_use", id: c2, name: "edit_file", input: edit },
          { type: "tool_use", id: c3, name: "shell", input: shell },
        ],
        a,
        "st2",
      );
      send(run, { type: "agent.step", agent_id: a, seq: 2, phase: "tools" });
      await sleep(200);
      // "ask": the edit goes through the permission engine — a rule in
      // the mock's files decides, else the client is asked.
      let editResult = EDIT_RESULT;
      let editDenied = false;
      if (prompt.includes("ask")) {
        const byRule = ruleDecides(s.info.workspace, "edit_file", undefined, edit.path);
        let decision: "allow" | "deny" = byRule ?? "allow";
        let source = "rule";
        let requestId: string | undefined;
        if (byRule === undefined) {
          const suggested: RuleSpec[] = [
            { tool: "edit_file", effect: "allow", match: { path: "src/**" } },
            { tool: "edit_file", effect: "allow", match: { outside_workspace: false } },
          ];
          const asked = await askPermission(s, run, "edit_file", edit, suggested);
          const answer = asked.answer;
          requestId = asked.requestId;
          source = answer === "timeout" || answer === "headless" ? answer : "user";
          decision = answer.startsWith("allow") ? "allow" : "deny";
        }
        const decided: Record<string, unknown> = {
          type: "permission.decision",
          agent_id: a,
          call_id: c2,
          tool: "edit_file",
          decision,
          source,
        };
        if (requestId !== undefined) decided.request_id = requestId;
        if (decision === "deny") decided.reason = `denied by ${source}`;
        send(run, decided);
        if (decision === "deny") {
          editDenied = true;
          editResult = `permission denied: ${String(decided.reason)}`;
        }
        if (run.cancelled) return finish("cancelled");
      }
      const ev2 = record(
        s,
        a,
        "tool.result",
        {
          call_id: c2,
          name: "edit_file",
          ok: !editDenied,
          output_bytes: editResult.length,
          media_type: "text/plain",
        },
        editResult,
      );
      send(run, {
        type: "agent.tool_result",
        agent_id: a,
        call_id: c2,
        name: "edit_file",
        ok: !editDenied,
        summary: editDenied ? editResult : "edited src/main.rs (+2 -1)",
        blob_id: `b_${ev2}`,
        mentor_bytes: editResult.length,
        event_id: ev2,
      });
      // The shell streams.
      const lines = prompt.includes("slow") ? 600 : 30;
      let out = "";
      for (let i = 0; i < lines && !run.cancelled; i++) {
        const line = i === 0 ? "   Compiling harness v0.1.0\n" : `test case_${i} ... ok\n`;
        out += line;
        send(run, {
          type: "agent.tool_progress",
          agent_id: a,
          call_id: c3,
          stream: i === 0 ? "stderr" : "stdout",
          text: line,
        });
        if (!document.hidden || i % 10 === 9) await sleep(prompt.includes("slow") ? 100 : 30);
      }
      if (run.cancelled) {
        send(run, {
          type: "agent.tool_result",
          agent_id: a,
          call_id: c3,
          name: "shell",
          ok: false,
          summary: "shell: cancelled",
        });
        push(
          s,
          "user",
          [
            {
              type: "tool_result",
              tool_use_id: c2,
              content: [{ type: "text", text: editResult }],
              is_error: editDenied,
            },
            {
              type: "tool_result",
              tool_use_id: c3,
              content: [{ type: "text", text: "cancelled" }],
              is_error: true,
            },
          ],
          a,
          "st2",
        );
        return finish("cancelled");
      }
      let raw = `${out}\ntest result: ok. ${lines} passed\n`;
      if (prompt.includes("big")) {
        const filler = "0123456789abcdef".repeat(4) + "\n";
        raw += filler.repeat(Math.ceil((5 * 1024 * 1024) / filler.length));
      }
      const mentorText =
        raw.length > 4000
          ? `${raw.slice(0, 4000)}\n[... ${raw.length - 4000} bytes omitted, full result id b_x]`
          : raw;
      const ev3 = record(
        s,
        a,
        "tool.result",
        {
          call_id: c3,
          name: "shell",
          ok: true,
          output_bytes: raw.length,
          media_type: "text/plain",
          duration_ms: 1234,
        },
        raw,
      );
      send(run, {
        type: "agent.tool_result",
        agent_id: a,
        call_id: c3,
        name: "shell",
        ok: true,
        summary: `exit 0 in 1.2 s`,
        blob_id: `b_${ev3}`,
        mentor_bytes: mentorText.length,
        event_id: ev3,
      });
      push(
        s,
        "user",
        [
          {
            type: "tool_result",
            tool_use_id: c2,
            content: [{ type: "text", text: editResult }],
            is_error: editDenied,
          },
          { type: "tool_result", tool_use_id: c3, content: [{ type: "text", text: mentorText }] },
        ],
        a,
        "st2",
      );
    }

    // Step 3: the answer.
    send(run, { type: "agent.step", agent_id: a, seq: 3, phase: "mentor" });
    if (prompt.includes("warn")) {
      send(run, {
        type: "agent.warning",
        agent_id: a,
        kind: "context_large",
        message: "the context is at 80% of the window",
      });
    }
    await stream(run, "agent.text_delta", ANSWER);
    usage(2600, 400);
    if (run.cancelled) return finish("cancelled");
    push(s, "assistant", [{ type: "text", text: ANSWER }], a, "st3");
    finish("ok");
    // The cheap model names the session a moment later: a mentor call too.
    setTimeout(() => {
      if (s.info.title_source !== "user") {
        s.info.title = "Load the config once";
        s.info.title_source = "generated";
        for (const c of calls) if (c.session_id === s.info.id) c.session_title = s.info.title;
        recordCall(s, a, "title", TITLE_MODEL, 80, 6);
      }
    }, 2000);
  }

  /**
   * Ten days of history on a demo workspace (a fresh mock only), so the
   * usage panel has something to chart: two sessions, a mix of models
   * and kinds, one failed call and one on a model without a price.
   */
  function seedHistory() {
    const ws = addWorkspace("C:/src/demo");
    const day = (n: number, h: number) => {
      const d = new Date();
      d.setDate(d.getDate() - n);
      d.setHours(h, 4, 0, 0);
      return d.toISOString();
    };
    const seedSession = (title: string, firstDay: number, prompt: string) => {
      const s = createSession(ws.root);
      s.info.title = title;
      s.info.title_source = "generated";
      s.info.created_at = day(firstDay, 9);
      s.info.updated_at = s.info.created_at;
      const a = nextId("a");
      s.agents.push({
        id: a,
        status: "ok",
        started_at: day(firstDay, 9),
        ended_at: day(firstDay, 9),
        model: "claude-opus-5",
        calls: 0,
        usage: zero,
      });
      push(s, "user", [{ type: "text", text: prompt }], a, undefined);
      push(s, "assistant", [{ type: "text", text: `Done: ${title.toLowerCase()}.` }], a, "st1");
      s.messages[0]!.created_at = day(firstDay, 9);
      s.messages[1]!.created_at = day(firstDay, 9);
      s.info.last_activity = day(firstDay, 9);
      return { s, a };
    };
    const one = seedSession("Split the parser module", 9, "split the parser into a module");
    const two = seedSession("Fix the flaky socket test", 4, "the socket test is flaky, fix it");
    for (const n of [9, 8, 7, 6, 5, 3, 2, 1]) {
      const { s, a } = n >= 5 ? one : two;
      const model = n % 3 === 0 ? "claude-sonnet-5" : "claude-opus-5";
      recordCall(s, a, "step", model, 3000 + n * 700, 200 + n * 40, {
        started_at: day(n, 10),
        cache_read: n * 1500,
      });
      recordCall(s, a, "step", model, 4200 + n * 300, 150 + n * 20, {
        started_at: day(n, 11),
        cache_read: n * 1800,
        cache_creation: 400,
      });
    }
    recordCall(one.s, one.a, "title", TITLE_MODEL, 80, 6, { started_at: day(9, 10) });
    recordCall(two.s, two.a, "title", TITLE_MODEL, 90, 7, { started_at: day(4, 10) });
    recordCall(two.s, two.a, "step", "claude-opus-5", 0, 0, {
      started_at: day(2, 12),
      status: "error",
      total_ms: 12_030,
    });
    recordCall(two.s, two.a, "step", "claude-example-1", 1500, 90, { started_at: day(1, 12) });
    calls.sort((x, y) => x.started_at.localeCompare(y.started_at));
    save();
  }

  // What the last page left, and its runs carried on.
  try {
    const raw = sessionStorage.getItem(STORE_KEY);
    if (raw !== null) {
      const state = JSON.parse(raw) as Persisted;
      ids = state.ids;
      for (const s of state.sessions) sessions.set(s.info.id, s);
      for (const e of state.trace) trace.set(e.event.id, e);
      calls.push(...(state.calls ?? []));
      for (const w of state.workspaces ?? []) workspaces.set(w.id, w);
      config = state.config ?? {};
      workspaceConfig = state.workspaceConfig ?? {};
      rules = state.rules ?? { user: [], workspace: {} };
      keyConfigured = state.keyConfigured ?? true;
      for (const r of state.runs) {
        const s = sessions.get(r.sessionId);
        if (s === undefined) continue;
        const run: Run = { ...r, channels: new Set(), seq: 0, cancelled: false };
        runs.set(r.agentId, run);
        void runScript(s, run, r.prompt);
      }
    }
  } catch (e) {
    console.warn("mock: cannot restore", e);
  }
  if (sessions.size === 0 && calls.length === 0) seedHistory();

  /** The daemon's range bounds: an age (`7d`, `12h`), a bare local date, or RFC 3339. */
  function bound(text: unknown, kind: "since" | "until"): string | undefined {
    if (typeof text !== "string" || text.trim() === "") return undefined;
    const age = /^(\d+)([mhdw])$/.exec(text.trim());
    if (age !== null) {
      const unit = { m: 60_000, h: 3_600_000, d: 86_400_000, w: 7 * 86_400_000 }[age[2]!]!;
      return new Date(Date.now() - Number(age[1]) * unit).toISOString();
    }
    const date = /^(\d{4})-(\d{2})-(\d{2})$/.exec(text.trim());
    if (date !== null) {
      const d = new Date(Number(date[1]), Number(date[2]) - 1, Number(date[3]));
      if (kind === "until") d.setDate(d.getDate() + 1);
      return d.toISOString();
    }
    const d = new Date(text);
    if (Number.isNaN(d.getTime())) {
      throw rpcError(-32602, "invalid_params", `cannot parse ${kind} "${text}"`);
    }
    return d.toISOString();
  }

  /** The calls of a `stats.*` query, oldest first. */
  function callsOf(params: Record<string, unknown>): {
    rows: CallSummary[];
    since?: string;
    until?: string;
  } {
    const since = bound(params.since, "since");
    const until = bound(params.until, "until");
    const rows = calls.filter(
      (c) =>
        (params.session_id === undefined || c.session_id === params.session_id) &&
        (params.workspace_id === undefined || c.workspace_id === params.workspace_id) &&
        (since === undefined || c.started_at >= since) &&
        (until === undefined || c.started_at < until),
    );
    const out: ReturnType<typeof callsOf> = { rows };
    if (since !== undefined) out.since = since;
    if (until !== undefined) out.until = until;
    return out;
  }

  function bucket(
    key: string | undefined,
    label: string | undefined,
    rows: CallSummary[],
  ): TokenBucket {
    const t = totalsOf(rows);
    const b: TokenBucket = {
      calls: t.calls,
      input: t.usage.input_tokens,
      output: t.usage.output_tokens,
      cache_read: t.usage.cache_read_input_tokens,
      cache_creation: t.usage.cache_creation_input_tokens,
      cost_usd: rows.reduce((acc, c) => acc + (c.cost_usd ?? 0), 0),
      unpriced_calls: t.unpriced,
    };
    if (key !== undefined) b.key = key;
    if (label !== undefined) b.label = label;
    return b;
  }

  /** Groups rows by `keyOf`, ordered by key or by cost (highest first). */
  function grouped(
    rows: CallSummary[],
    keyOf: (c: CallSummary) => string,
    labelOf: (c: CallSummary) => string | undefined,
    byCost: boolean,
  ): TokenBucket[] {
    const groups = new Map<string, CallSummary[]>();
    for (const c of rows) {
      const k = keyOf(c);
      groups.set(k, [...(groups.get(k) ?? []), c]);
    }
    const out = [...groups.entries()].map(([k, rs]) => bucket(k, labelOf(rs[0]!), rs));
    out.sort((a, b) =>
      byCost && a.cost_usd !== b.cost_usd
        ? b.cost_usd - a.cost_usd
        : (a.key ?? "").localeCompare(b.key ?? ""),
    );
    return out;
  }

  const localDay = (ts: string) => {
    const d = new Date(ts);
    const p = (n: number) => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  };
  const tz = () => {
    const off = -new Date().getTimezoneOffset();
    const p = (n: number) => String(Math.abs(n)).padStart(2, "0");
    return `${off < 0 ? "-" : "+"}${p(Math.trunc(off / 60))}:${p(off % 60)}`;
  };

  function call(method: string, params: Record<string, unknown>): unknown {
    const session = (id: unknown) => {
      const s = sessions.get(String(id));
      if (s === undefined) throw rpcError(-32004, "not_found", `session ${String(id)} not found`);
      return s;
    };
    const workspaceOf = (id: unknown) => {
      const w = workspaces.get(String(id));
      if (w === undefined) throw rpcError(-32004, "not_found", `workspace ${String(id)} not found`);
      return w;
    };
    const idle = (s: Session) => {
      if ([...runs.values()].some((r) => r.sessionId === s.info.id)) {
        throw rpcError(-32009, "conflict", "an agent is still running on this session");
      }
    };
    const layerFile = (layer: unknown, workspace: unknown) => {
      if (layer === "workspace") {
        if (typeof workspace !== "string") {
          throw rpcError(-32602, "invalid_params", "the workspace layer needs `workspace`");
        }
        return (rules.workspace[workspace] ??= []);
      }
      return rules.user;
    };
    switch (method) {
      case "daemon.status":
        return {
          version: "mock",
          pid: 0,
          uptime_s: Math.floor(performance.now() / 1000),
          sessions_open: sessions.size,
          data_dir: "(mock)",
        };
      case "auth.status":
        return {
          providers: [
            keyConfigured
              ? { name: "anthropic", configured: true, source: "mock" }
              : { name: "anthropic", configured: false },
          ],
        };
      case "auth.set_key":
        if (String(params.key ?? "").trim() === "") {
          throw rpcError(-32602, "invalid_params", "key must not be empty");
        }
        keyConfigured = true;
        save();
        return {};
      case "config.path":
        return { config_file: "(mock)/config.toml", data_dir: "(mock)/data" };
      case "config.get": {
        const key = typeof params.key === "string" ? params.key : undefined;
        const ws = typeof params.workspace === "string" ? params.workspace : undefined;
        if (key === undefined) return { value: { ...DEFAULTS, ...config } };
        if (ws !== undefined && workspaceConfig[ws]?.[key] !== undefined) {
          return { value: workspaceConfig[ws][key], source: "workspace" };
        }
        if (config[key] !== undefined) return { value: config[key], source: "user" };
        if (key in DEFAULTS) return { value: DEFAULTS[key], source: "default" };
        throw rpcError(-32004, "not_found", `unknown key ${key}`);
      }
      case "config.set": {
        const key = String(params.key);
        if (!(key in DEFAULTS)) throw rpcError(-32050, "config", `unknown key ${key}`);
        if (params.layer === "workspace") {
          if (!WORKSPACE_KEYS.test(key)) {
            throw rpcError(-32050, "config", `${key} cannot be set per workspace`);
          }
          const ws = String(params.workspace);
          const table = (workspaceConfig[ws] ??= {});
          if (params.value === null) delete table[key];
          else table[key] = params.value;
        } else if (params.value === null) delete config[key];
        else config[key] = params.value;
        save();
        return {};
      }
      case "workspace.list":
        return {
          workspaces: [...workspaces.values()].sort((a, b) =>
            b.last_used_at.localeCompare(a.last_used_at),
          ),
        };
      case "workspace.add":
        if (typeof params.root !== "string" || params.root.trim() === "") {
          throw rpcError(-32602, "invalid_params", "root must be an absolute path");
        }
        return addWorkspace(params.root, typeof params.name === "string" ? params.name : undefined);
      case "workspace.remove": {
        const w = workspaceOf(params.id);
        workspaces.delete(w.id);
        let unlinked = 0;
        for (const s of sessions.values()) {
          if (s.info.workspace_id === w.id) {
            s.info.workspace_id = null;
            unlinked += 1;
          }
        }
        save();
        return { sessions_unlinked: unlinked };
      }
      case "workspace.info":
      case "workspace.refresh": {
        const w = workspaceOf(params.id);
        return {
          ...w,
          file_count: 1234,
          index_age_s: 3,
          git_head: "0123456789abcdef0123456789abcdef01234567",
          git_branch: "main",
          git_dirty: w.name.length % 2 === 0,
          has_instructions: true,
          has_config: Object.keys(workspaceConfig[w.root] ?? {}).length > 0,
          has_ignore_file: false,
          config_overrides: Object.keys(workspaceConfig[w.root] ?? {}),
        };
      }
      case "session.create": {
        const s = createSession(
          typeof params.workspace === "string" ? params.workspace : undefined,
        );
        return { session_id: s.info.id };
      }
      case "session.list": {
        const q = typeof params.query === "string" ? params.query.toLowerCase() : "";
        const rows = [...sessions.values()]
          .filter((s) => params.include_archived === true || s.info.status === "open")
          .filter(
            (s) => params.workspace_id === undefined || s.info.workspace_id === params.workspace_id,
          )
          .filter(
            (s) =>
              q === "" ||
              (s.info.title ?? "").toLowerCase().includes(q) ||
              s.messages.some((m) => textOf(m).toLowerCase().includes(q)),
          )
          .map(summary)
          .sort((a, b) => b.last_activity.localeCompare(a.last_activity));
        return { sessions: rows };
      }
      case "session.search": {
        const words = String(params.query ?? "")
          .toLowerCase()
          .split(/\s+/)
          .filter(Boolean);
        if (words.length === 0) return { hits: [] };
        const hits: SessionSearchHit[] = [];
        for (const s of sessions.values()) {
          if (params.include_archived !== true && s.info.status !== "open") continue;
          for (const m of s.messages) {
            const text = textOf(m);
            const lower = text.toLowerCase();
            if (!words.every((w) => lower.includes(w))) continue;
            const at = lower.indexOf(words[0]!);
            const start = Math.max(0, at - 30);
            let snippet = text.slice(start, at + 60).replace(/\s+/g, " ");
            for (const w of words) snippet = snippet.replace(new RegExp(w, "i"), (x) => `[${x}]`);
            const hit: SessionSearchHit = {
              session_id: s.info.id,
              seq: m.seq,
              role: m.role,
              snippet: `${start > 0 ? "…" : ""}${snippet}…`,
              created_at: m.created_at,
            };
            if (s.info.title !== undefined && s.info.title !== null) hit.title = s.info.title;
            if (s.info.workspace !== undefined && s.info.workspace !== null) {
              hit.workspace = s.info.workspace;
            }
            hits.push(hit);
          }
        }
        hits.sort((a, b) => b.created_at.localeCompare(a.created_at));
        return { hits: hits.slice(0, typeof params.limit === "number" ? params.limit : 50) };
      }
      case "session.rename": {
        const s = session(params.id);
        s.info.title = String(params.title);
        s.info.title_source = "user";
        save();
        return {};
      }
      case "session.archive": {
        const s = session(params.id);
        idle(s);
        s.info.status = params.archived === false ? "open" : "archived";
        save();
        return {};
      }
      case "session.delete": {
        const s = session(params.id);
        idle(s);
        const n = s.messages.length;
        if (params.purge_traces === true) sessions.delete(s.info.id);
        else {
          s.messages = [];
          s.info.message_count = 0;
          s.info.status = "deleted";
        }
        save();
        return { messages_deleted: n, events_deleted: 0 };
      }
      case "session.export": {
        const s = session(params.id);
        return {
          format: "harness-session/1",
          exported_at: now(),
          session: summary(s),
          messages: s.messages,
          mentor_calls: [],
        };
      }
      case "tools.list":
        return {
          tools: MOCK_TOOLS.map((name) => ({
            name,
            description: `${name} (mock)`,
            input_schema: {},
            risk:
              name === "shell"
                ? "execute"
                : name.startsWith("edit") || name.startsWith("write")
                  ? "write"
                  : "read_only",
            enabled: true,
          })),
        };
      case "tools.rules": {
        const ws = typeof params.workspace === "string" ? params.workspace : undefined;
        const list: RuleInfo[] = [];
        const files = [];
        if (ws !== undefined) {
          const wsRules = rules.workspace[ws] ?? [];
          list.push(
            ...wsRules.map((rule, i) => ({
              source: "workspace" as const,
              index: i + 1,
              line: i * 4 + 2,
              rule,
            })),
          );
          files.push({
            source: "workspace",
            path: `${ws}/.harness/permissions.toml`,
            exists: wsRules.length > 0,
          });
        }
        list.push(
          ...rules.user.map((rule, i) => ({
            source: "user" as const,
            index: i + 1,
            line: i * 4 + 2,
            rule,
          })),
        );
        files.push({
          source: "user",
          path: "(mock)/permissions.toml",
          exists: rules.user.length > 0,
          default: "ask",
        });
        list.push(...BUILTIN_RULES);
        return { rules: list, files };
      }
      case "tools.allow":
      case "tools.deny": {
        const file = layerFile(params.layer, params.workspace);
        const tool = String(params.tool);
        if (tool === "" || /\s/.test(tool)) {
          throw rpcError(-32602, "invalid_params", "`tool` must be a tool name or `*`");
        }
        const rule: RuleSpec = { tool, effect: method === "tools.deny" ? "deny" : "allow" };
        if (params.match !== undefined && Object.keys(params.match as object).length > 0) {
          rule.match = params.match as NonNullable<RuleSpec["match"]>;
        }
        file.push(rule);
        save();
        return {
          path:
            params.layer === "workspace"
              ? `${String(params.workspace)}/.harness/permissions.toml`
              : "(mock)/permissions.toml",
          line: (file.length - 1) * 4 + 2,
          rule,
        };
      }
      case "tools.remove": {
        const file = layerFile(params.layer, params.workspace);
        const index = Number(params.index);
        if (!(index >= 1 && index <= file.length)) {
          throw rpcError(-32050, "config", `no rule ${index}: the file has ${file.length}`);
        }
        const [rule] = file.splice(index - 1, 1);
        save();
        return { path: "(mock)/permissions.toml", rule };
      }
      case "permission.respond": {
        const ask = asks.get(String(params.request_id));
        if (ask === undefined) {
          throw rpcError(-32004, "not_found", "no such request (answered already, or timed out)");
        }
        const answer = String(params.answer);
        const s = sessions.get(ask.sessionId);
        // The lasting answers write a rule.
        const rule = (params.rule ?? undefined) as RuleSpec | undefined;
        const written: RuleSpec | undefined =
          rule === undefined
            ? undefined
            : { ...rule, effect: answer === "deny_always" ? "deny" : "allow" };
        if (written !== undefined) {
          if (answer === "allow_workspace" && s?.info.workspace != null) {
            (rules.workspace[s.info.workspace] ??= []).push(written);
          } else if (answer === "allow_always" || answer === "deny_always") {
            rules.user.push(written);
          }
          save();
        }
        ask.resolve(answer);
        return {};
      }
      case "session.get": {
        const s = session(params.id);
        const limit = typeof params.limit === "number" ? params.limit : 200;
        let rows: SessionMessage[];
        let hasMore: boolean;
        if (typeof params.before_seq === "number") {
          const before = s.messages.filter((m) => m.seq < (params.before_seq as number));
          rows = before.slice(-limit);
          hasMore = before.length > limit;
        } else {
          const after = typeof params.after_seq === "number" ? params.after_seq : 0;
          const from = s.messages.filter((m) => m.seq > after);
          rows = from.slice(0, limit);
          hasMore = from.length > limit;
        }
        const out: SessionGetResult = {
          session: summary(s),
          messages: rows.map((m) => ({ ...m })),
          has_more: hasMore,
          agents: s.agents.map((a) => ({ ...a })),
        };
        return out;
      }
      case "agent.cancel": {
        const run = runs.get(String(params.agent_id));
        if (run !== undefined) run.cancelled = true;
        return {};
      }
      case "stats.tokens": {
        const q = callsOf(params);
        const asked = Array.isArray(params.group_by) ? (params.group_by as StatsGroup[]) : [];
        const groups = new Set<StatsGroup>(
          asked.length === 0 ? ["model", "day", "session"] : asked,
        );
        const by = (g: StatsGroup, buckets: () => TokenBucket[]) =>
          groups.has(g) ? buckets() : [];
        return {
          range: { since: q.since ?? null, until: q.until ?? null },
          tz: tz(),
          totals: bucket(undefined, undefined, q.rows),
          by_model: by("model", () =>
            grouped(
              q.rows,
              (c) => c.model,
              () => undefined,
              false,
            ),
          ),
          by_day: by("day", () =>
            grouped(
              q.rows,
              (c) => localDay(c.started_at),
              () => undefined,
              false,
            ),
          ),
          by_session:
            params.session_id === undefined
              ? by("session", () =>
                  grouped(
                    q.rows,
                    (c) => c.session_id,
                    (c) => c.session_title,
                    true,
                  ),
                )
              : [],
          by_workspace: by("workspace", () =>
            grouped(
              q.rows,
              (c) => c.workspace_id ?? "",
              (c) =>
                c.workspace_id === undefined ? undefined : workspaces.get(c.workspace_id)?.root,
              true,
            ),
          ),
          by_kind: by("kind", () =>
            grouped(
              q.rows,
              (c) => c.kind,
              () => undefined,
              false,
            ),
          ),
          apprentice: {
            invocations: 0,
            bypassed: 0,
            tokens_in: 0,
            tokens_out: 0,
            estimated_saved_input: 0,
          },
        };
      }
      case "stats.calls": {
        const q = callsOf(params);
        const limit = Math.min(1000, Math.max(1, Number(params.limit ?? 100)));
        const offset = Number(params.offset ?? 0);
        const newest = [...q.rows].reverse();
        return { calls: newest.slice(offset, offset + limit), total: newest.length };
      }
      case "trace.list": {
        const kinds = Array.isArray(params.kinds) ? (params.kinds as string[]) : undefined;
        const events: EventSummary[] = [...trace.values()]
          .map((e) => e.event)
          .filter(
            (e) =>
              (params.session_id === undefined || e.session_id === params.session_id) &&
              (params.agent_id === undefined || e.agent_id === params.agent_id) &&
              (kinds === undefined || kinds.includes(e.kind)),
          )
          .map(({ id, session_id, agent_id, seq, ts, kind, blob_bytes }) => ({
            id,
            session_id,
            agent_id: agent_id ?? null,
            seq,
            ts,
            kind,
            blob_bytes: blob_bytes ?? null,
          }));
        return { events };
      }
      case "trace.get": {
        const e = trace.get(String(params.event_id));
        if (e === undefined) throw rpcError(-32004, "not_found", "event not found");
        return params.include_blob === true && e.blob !== undefined
          ? { event: e.event, blob: e.blob }
          : { event: e.event };
      }
      default:
        throw rpcError(-32601, "method_not_found", `${method} is not in the mock`);
    }
  }

  function startStream(method: string, params: Record<string, unknown>, channel: string): unknown {
    if (method === "agent.run") {
      const s = sessions.get(String(params.session_id));
      if (s === undefined) throw rpcError(-32004, "not_found", "session not found");
      if ([...runs.values()].some((r) => r.sessionId === s.info.id)) {
        throw rpcError(-32009, "conflict", "an agent is still running on this session");
      }
      const prompt = String(params.prompt);
      const agentId = nextId("a");
      const run: Run = {
        agentId,
        sessionId: s.info.id,
        prompt,
        channels: new Set([channel]),
        seq: 0,
        cancelled: false,
      };
      runs.set(agentId, run);
      s.agents.push({ id: agentId, status: "running", started_at: now(), calls: 0, usage: zero });
      if (s.info.title === undefined) {
        const line = prompt.split("\n", 1)[0] ?? "";
        s.info.title = line.length > 60 ? `${line.slice(0, 60)}…` : line;
        s.info.title_source = "prompt";
      }
      push(s, "user", [{ type: "text", text: prompt }], agentId);
      void runScript(s, run, prompt);
      return { subscription: agentId, result: { agent_id: agentId, subscription: agentId } };
    }
    if (method === "agent.subscribe") {
      const agentId = String(params.agent_id);
      const run = runs.get(agentId);
      if (run !== undefined) {
        run.channels.add(channel);
        return { subscription: agentId, result: { subscription: agentId, running: true } };
      }
      const s = [...sessions.values()].find((x) => x.agents.some((a) => a.id === agentId));
      const status = s?.agents.find((a) => a.id === agentId)?.status ?? "error";
      setTimeout(() => {
        emit(`rpc:event:${channel}`, {
          subscription: agentId,
          seq: 1,
          event: {
            type: "agent.finished",
            agent_id: agentId,
            status: status === "running" ? "error" : status,
          },
        });
      }, 0);
      return { subscription: agentId, result: { subscription: agentId, running: false } };
    }
    throw rpcError(-32601, "method_not_found", `${method} is not a streaming method in the mock`);
  }

  return {
    async invoke<T>(command: string, args: Record<string, unknown>): Promise<T> {
      await sleep(20);
      switch (command) {
        case "daemon_status":
          return { connected: true, version: "mock", pid: 0, spawned: false } as T;
        case "app_info":
          return { version: "mock", config_file: "(mock)", data_dir: "(mock)" } as T;
        case "daemon_restart":
          return undefined as T;
        case "open_path":
          console.info("mock: open", args.path);
          return undefined as T;
        case "write_text_file":
          console.info("mock: write", args.path, String(args.contents).length, "bytes");
          return undefined as T;
        case "dir_size":
          return 123_456_789 as T;
        case "quit_and_stop_daemon":
          console.info("mock: quit");
          return undefined as T;
        case "rpc_call":
          return call(String(args.method), (args.params ?? {}) as Record<string, unknown>) as T;
        case "rpc_stream":
          return startStream(
            String(args.method),
            (args.params ?? {}) as Record<string, unknown>,
            String(args.channel),
          ) as T;
        default:
          throw rpcError(-32601, "bridge", `unknown command ${command}`);
      }
    },
    async listen<T>(event: string, handler: EventHandler<T>): Promise<() => void> {
      let set = listeners.get(event);
      if (set === undefined) {
        set = new Set();
        listeners.set(event, set);
      }
      const h = handler as EventHandler<unknown>;
      set.add(h);
      if (event === "daemon:status") {
        setTimeout(() => h({ connected: true, version: "mock", pid: 0, spawned: false }), 0);
      }
      return () => {
        set.delete(h);
      };
    },
  };
}
