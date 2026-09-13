// A scripted daemon for running the page outside Tauri (`pnpm dev` in a
// browser): enough of the RPC surface for the chat view — sessions with
// stored rows and agents, `agent.run` streaming a canned answer with
// thinking, markdown, a read, an edit (with a diff) and a shell call
// with streamed output, `agent.cancel`, `agent.subscribe`, and the trace
// events behind the Raw tab. Words in the prompt steer the script:
// "fail" ends the run with an error before any tool, "slow" makes the
// shell run for a minute (try Cancel), "big" gives it 5 MB of output.
// The state lives in `sessionStorage` (blobs aside), so a reload finds
// its sessions again and a run in progress carries on — the reattach
// path. Nothing here is used in the app: `bridge.ts` loads it only when
// `isTauri()` is false.

import type {
  AgentSummary,
  EventNotification,
  EventSummary,
  RpcError,
  SessionGetResult,
  SessionInfo,
  SessionMessage,
  TraceEvent,
  Usage,
} from "./api";
import type { Backend, EventHandler } from "./bridge";

interface Session {
  info: SessionInfo;
  messages: SessionMessage[];
  agents: AgentSummary[];
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
}

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

  function createSession(workspace: string | undefined): Session {
    const id = nextId("s");
    const ts = now();
    const info: SessionInfo = {
      id,
      workspace: workspace ?? null,
      status: "open",
      created_at: ts,
      updated_at: ts,
      message_count: 0,
      last_activity: ts,
      usage: zero,
      config: {},
    };
    const s = { info, messages: [], agents: [] };
    sessions.set(id, s);
    return s;
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
      turn.cost_usd = (turn.cost_usd ?? 0) + input * 0.000005 + output * 0.000025;
      send(run, {
        type: "agent.usage",
        agent_id: a,
        call_id: nextId("m"),
        usage: { ...zero, input_tokens: input, output_tokens: output },
        cost_usd: input * 0.000005 + output * 0.000025,
      });
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
      const ev2 = record(
        s,
        a,
        "tool.result",
        {
          call_id: c2,
          name: "edit_file",
          ok: true,
          output_bytes: EDIT_RESULT.length,
          media_type: "text/plain",
        },
        EDIT_RESULT,
      );
      send(run, {
        type: "agent.tool_result",
        agent_id: a,
        call_id: c2,
        name: "edit_file",
        ok: true,
        summary: "edited src/main.rs (+2 -1)",
        blob_id: `b_${ev2}`,
        mentor_bytes: EDIT_RESULT.length,
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
              content: [{ type: "text", text: EDIT_RESULT }],
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
          { type: "tool_result", tool_use_id: c2, content: [{ type: "text", text: EDIT_RESULT }] },
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
    // The cheap model names the session a moment later.
    setTimeout(() => {
      if (s.info.title_source !== "user") {
        s.info.title = "Load the config once";
        s.info.title_source = "generated";
        save();
      }
    }, 2000);
  }

  // What the last page left, and its runs carried on.
  try {
    const raw = sessionStorage.getItem(STORE_KEY);
    if (raw !== null) {
      const state = JSON.parse(raw) as Persisted;
      ids = state.ids;
      for (const s of state.sessions) sessions.set(s.info.id, s);
      for (const e of state.trace) trace.set(e.event.id, e);
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

  function call(method: string, params: Record<string, unknown>): unknown {
    const session = (id: unknown) => {
      const s = sessions.get(String(id));
      if (s === undefined) throw rpcError(-32004, "not_found", `session ${String(id)} not found`);
      return s;
    };
    switch (method) {
      case "auth.status":
        return { providers: [{ name: "anthropic", configured: true, source: "mock" }] };
      case "config.get":
        if (params.key === "mentor.model") return { value: "claude-opus-5", source: "default" };
        if (params.key === "mentor.thinking_display")
          return { value: "summarized", source: "default" };
        return { value: null };
      case "session.create": {
        const s = createSession(
          typeof params.workspace === "string" ? params.workspace : undefined,
        );
        return { session_id: s.info.id };
      }
      case "session.list":
        return { sessions: [...sessions.values()].map((s) => s.info) };
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
          session: { ...s.info },
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
