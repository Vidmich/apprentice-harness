import { describe, expect, it } from "vitest";
import type {
  Event,
  EventNotification,
  SessionGetResult,
  SessionInfo,
  SessionMessage,
} from "./api";
import {
  type Item,
  type Transcript,
  applyEvent,
  attachLive,
  cancellingLive,
  clearLive,
  deriveItems,
  emptyTranscript,
  failLive,
  isBusy,
  mergeStored,
  promptOf,
  startLive,
} from "./transcript";

const info: SessionInfo = {
  id: "s1",
  title: "add hello",
  workspace: "C:/src/repo",
  status: "open",
  created_at: "2026-09-12T10:00:00.000Z",
  updated_at: "2026-09-12T10:00:00.000Z",
  message_count: 0,
  calls: 0,
  last_activity: "2026-09-12T10:00:00.000Z",
  usage: {
    input_tokens: 0,
    output_tokens: 0,
    cache_read_input_tokens: 0,
    cache_creation_input_tokens: 0,
  },
  config: {},
};

function row(
  seq: number,
  role: "user" | "assistant",
  content: unknown[],
  agent = "a1",
  step?: string,
): SessionMessage {
  const m: SessionMessage = {
    seq,
    role,
    content,
    agent_id: agent,
    created_at: `2026-09-12T10:00:${String(seq).padStart(2, "0")}.000Z`,
  };
  if (step !== undefined) m.step_id = step;
  return m;
}

function page(messages: SessionMessage[], extra: Partial<SessionGetResult> = {}): SessionGetResult {
  return { session: info, messages, ...extra };
}

let seq = 0;
function ev(event: Event, subscription = "a1"): EventNotification {
  return { subscription, seq: ++seq, event };
}

const kinds = (items: Item[]) => items.map((i) => i.kind);

/** A stored conversation: prompt, a read, its result, the answer. */
const stored: SessionMessage[] = [
  row(1, "user", [{ type: "text", text: "add hello" }]),
  row(
    2,
    "assistant",
    [
      { type: "thinking", thinking: "look first", signature: "sig" },
      { type: "text", text: "Reading." },
      { type: "tool_use", id: "c1", name: "read_file", input: { path: "src/main.rs" } },
    ],
    "a1",
    "st1",
  ),
  row(
    3,
    "user",
    [{ type: "tool_result", tool_use_id: "c1", content: [{ type: "text", text: "fn main() {}" }] }],
    "a1",
    "st1",
  ),
  row(4, "assistant", [{ type: "text", text: "Done: **hello**." }], "a1", "st2"),
];

describe("stored rows", () => {
  it("become items with tool results folded into their cards and a footer per agent", () => {
    const t = mergeStored(
      emptyTranscript("s1"),
      page(stored, {
        has_more: true,
        agents: [
          {
            id: "a1",
            status: "ok",
            started_at: "2026-09-12T10:00:01.000Z",
            ended_at: "2026-09-12T10:00:09.000Z",
            model: "claude-opus-5",
            calls: 2,
            usage: {
              input_tokens: 10,
              output_tokens: 5,
              cache_read_input_tokens: 0,
              cache_creation_input_tokens: 0,
            },
            cost_usd: 0.01,
          },
        ],
      }),
      "newest",
    );
    expect(t.hasOlder).toBe(true);
    const items = deriveItems(t);
    expect(kinds(items)).toEqual([
      "user",
      "thinking",
      "assistant_text",
      "tool_call",
      "assistant_text",
      "turn_footer",
    ]);
    const card = items[3];
    expect(card?.kind === "tool_call" && card.result).toEqual({
      text: "fn main() {}",
      isError: false,
    });
    expect(card?.kind === "tool_call" && card.input).toEqual({ path: "src/main.rs" });
    const footer = items[5];
    expect(footer?.kind === "turn_footer" && footer.turn.model).toBe("claude-opus-5");
    expect(footer?.kind === "turn_footer" && footer.toolCalls).toBe(1);
    expect(footer?.kind === "turn_footer" && footer.turn.endedAt).toBe(
      Date.parse("2026-09-12T10:00:09.000Z"),
    );
    expect(promptOf(t, "a1")).toBe("add hello");
  });

  it("pages older rows in front and replaces a user turn that grew", () => {
    let t = mergeStored(emptyTranscript("s1"), page(stored.slice(2), { has_more: true }), "newest");
    expect(t.messages.map((m) => m.seq)).toEqual([3, 4]);
    t = mergeStored(t, page(stored.slice(0, 2), { has_more: false }), "older");
    expect(t.messages.map((m) => m.seq)).toEqual([1, 2, 3, 4]);
    expect(t.hasOlder).toBe(false);
    // The last user row grew (a prompt joined the tool results) — same seq, new content.
    const grown = row(
      3,
      "user",
      [
        { type: "tool_result", tool_use_id: "c1", content: "fn main() {}" },
        { type: "text", text: "and more" },
      ],
      "a2",
    );
    t = mergeStored(t, page([grown]), "refresh");
    expect(t.messages.length).toBe(4);
    expect(t.hasOlder).toBe(false);
    const items = deriveItems(t);
    expect(
      items.filter((i) => i.kind === "user").map((i) => (i.kind === "user" ? i.text : "")),
    ).toEqual(["add hello", "and more"]);
    // Synthetic user texts are marked.
    const synthetic = mergeStored(
      t,
      page([
        row(5, "user", [{ type: "text", text: "[continue — output was cut off]" }], "a2", "st3"),
      ]),
      "refresh",
    );
    const last = deriveItems(synthetic).find(
      (i) => i.kind === "user" && i.text.startsWith("[continue"),
    );
    expect(last?.kind === "user" && last.synthetic).toBe(true);
    expect(promptOf(synthetic, "a2")).toBe("and more");
  });
});

describe("the live run", () => {
  function run(): Transcript {
    let t = mergeStored(
      emptyTranscript("s1"),
      page(stored, {
        agents: [
          {
            id: "a1",
            status: "ok",
            started_at: "2026-09-12T10:00:01.000Z",
            ended_at: "2026-09-12T10:00:04.000Z",
            model: "claude-opus-5",
            calls: 2,
            usage: {
              input_tokens: 10,
              output_tokens: 5,
              cache_read_input_tokens: 0,
              cache_creation_input_tokens: 0,
            },
          },
        ],
      }),
      "newest",
    );
    t = startLive(t, "now test it", 1000);
    expect(isBusy(t)).toBe(true);
    t = attachLive(t, "a2", "a2", 1001);
    return t;
  }

  it("streams thinking, text and tool cards, then hands over to the stored rows", () => {
    let t = run();
    const send = (e: Event) => (t = applyEvent(t, ev(e, "a2"), 2000));
    send({ type: "agent.started", agent_id: "a2", session_id: "s1" });
    send({ type: "agent.step", agent_id: "a2", seq: 1, phase: "mentor" });
    send({ type: "agent.thinking_delta", agent_id: "a2", text: "hmm" });
    send({ type: "agent.text_delta", agent_id: "a2", text: "Let me " });
    send({ type: "agent.text_delta", agent_id: "a2", text: "run it." });
    let items = deriveItems(t);
    expect(kinds(items).slice(-5)).toEqual([
      "turn_footer",
      "user",
      "thinking",
      "assistant_text",
      "turn_footer",
    ]);
    const pending = items.find((i) => i.kind === "user" && i.pending);
    expect(pending?.kind === "user" && pending.text).toBe("now test it");
    const text = items[items.length - 2];
    expect(text?.kind === "assistant_text" && text.text).toBe("Let me run it.");
    expect(text?.kind === "assistant_text" && text.streaming).toBe(true);

    send({
      type: "agent.tool_call",
      agent_id: "a2",
      call_id: "c2",
      name: "shell",
      input: { command: "cargo test" },
    });
    send({ type: "agent.step", agent_id: "a2", seq: 1, phase: "tools" });
    send({
      type: "agent.tool_progress",
      agent_id: "a2",
      call_id: "c2",
      stream: "stdout",
      text: "running 1 test\n",
    });
    send({
      type: "agent.tool_progress",
      agent_id: "a2",
      call_id: "c2",
      stream: "stdout",
      text: "ok\n",
    });
    items = deriveItems(t);
    const card = items.find((i) => i.kind === "tool_call" && i.callId === "c2");
    expect(card?.kind === "tool_call" && card.meta?.status).toBe("running");
    expect(card?.kind === "tool_call" && card.meta?.progress.map((p) => p.text)).toEqual([
      "running 1 test\n",
      "ok\n",
    ]);
    expect(card?.kind === "tool_call" && card.live).toBe(true);
    // The text item is no longer streaming once a tool call follows it.
    const t1 = items.find((i) => i.kind === "assistant_text" && i.key === "live:1:text");
    expect(t1?.kind === "assistant_text" && t1.streaming).toBe(false);

    send({
      type: "agent.tool_result",
      agent_id: "a2",
      call_id: "c2",
      name: "shell",
      ok: true,
      summary: "exit 0 in 1.2 s",
      blob_id: "b1",
      mentor_bytes: 18,
      event_id: "ev9",
    });
    send({
      type: "agent.usage",
      agent_id: "a2",
      call_id: "m1",
      usage: {
        input_tokens: 100,
        output_tokens: 20,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
      },
      cost_usd: 0.002,
      session_usage: {
        input_tokens: 110,
        output_tokens: 25,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
      },
      session_cost_usd: 0.0138,
      session_calls: 3,
    });
    expect(t.calls.c2?.eventId).toBe("ev9");
    expect(t.calls.c2?.status).toBe("ok");
    expect(t.agents.a2?.usage.input_tokens).toBe(100);
    expect(t.agents.a2?.calls).toBe(1);
    // The header's session totals come from the event, not a refetch.
    expect(t.info?.usage.input_tokens).toBe(110);
    expect(t.info?.cost_usd).toBe(0.0138);
    expect(t.info?.calls).toBe(3);

    // Step 2 begins: the run refreshes and the stored rows of step 1 arrive.
    send({ type: "agent.step", agent_id: "a2", seq: 2, phase: "mentor" });
    expect(t.live?.steps.length).toBe(2);
    t = mergeStored(
      t,
      page(
        [
          row(5, "user", [{ type: "text", text: "now test it" }], "a2"),
          row(
            6,
            "assistant",
            [
              { type: "thinking", thinking: "hmm", signature: "s" },
              { type: "text", text: "Let me run it." },
              { type: "tool_use", id: "c2", name: "shell", input: { command: "cargo test" } },
            ],
            "a2",
            "st3",
          ),
          row(
            7,
            "user",
            [{ type: "tool_result", tool_use_id: "c2", content: "running 1 test\nok\n" }],
            "a2",
            "st3",
          ),
        ],
        {
          agents: [
            {
              id: "a2",
              status: "running",
              started_at: "2026-09-12T10:00:05.000Z",
              calls: 1,
              usage: {
                input_tokens: 100,
                output_tokens: 20,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
              },
              cost_usd: 0.002,
            },
          ],
        },
      ),
      "refresh",
    );
    expect(t.live?.prompt).toBeUndefined();
    expect(t.live?.steps.map((s) => s.seq)).toEqual([2]);
    items = deriveItems(t);
    // No duplicate: the card is the stored one now, with its result and its live meta.
    const cards = items.filter((i) => i.kind === "tool_call" && i.callId === "c2");
    expect(cards.length).toBe(1);
    const c2 = cards[0];
    expect(c2?.kind === "tool_call" && c2.result?.text).toBe("running 1 test\nok\n");
    expect(c2?.kind === "tool_call" && c2.meta?.summary).toBe("exit 0 in 1.2 s");
    expect(c2?.kind === "tool_call" && c2.live).toBe(false);
    expect(kinds(items).filter((k) => k === "user")).toHaveLength(2);

    send({ type: "agent.text_delta", agent_id: "a2", text: "All green." });
    send({
      type: "agent.usage",
      agent_id: "a2",
      call_id: "m2",
      usage: {
        input_tokens: 150,
        output_tokens: 5,
        cache_read_input_tokens: 100,
        cache_creation_input_tokens: 0,
      },
      cost_usd: 0.001,
    });
    send({ type: "agent.finished", agent_id: "a2", status: "ok" });
    expect(isBusy(t)).toBe(false);
    expect(t.agents.a2?.status).toBe("ok");
    expect(t.agents.a2?.usage).toEqual({
      input_tokens: 250,
      output_tokens: 25,
      cache_read_input_tokens: 100,
      cache_creation_input_tokens: 0,
    });
    expect(t.agents.a2?.costUsd).toBeCloseTo(0.003);
    expect(t.agents.a2?.endedAt).toBe(2000);
    // Nothing after the terminal event changes the run.
    const frozen = t;
    send({ type: "agent.text_delta", agent_id: "a2", text: "late" });
    expect(t).toBe(frozen);
    // The final refresh stores the answer; the overlay is empty and goes.
    t = mergeStored(
      t,
      page([row(8, "assistant", [{ type: "text", text: "All green." }], "a2", "st4")]),
      "refresh",
    );
    expect(t.live?.steps).toEqual([]);
    t = clearLive(t);
    expect(t.live).toBeUndefined();
    items = deriveItems(t);
    expect(kinds(items)).toEqual([
      "user",
      "thinking",
      "assistant_text",
      "tool_call",
      "assistant_text",
      "turn_footer",
      "user",
      "thinking",
      "assistant_text",
      "tool_call",
      "assistant_text",
      "turn_footer",
    ]);
    const footer = items[items.length - 1];
    expect(footer?.kind === "turn_footer" && footer.turn.status).toBe("ok");
    expect(footer?.kind === "turn_footer" && footer.turn.calls).toBe(2);
  });

  it("ignores events of another subscription and unknown types", () => {
    let t = run();
    const before = t;
    t = applyEvent(t, ev({ type: "agent.text_delta", agent_id: "a9", text: "x" }, "a9"), 1);
    expect(t).toBe(before);
    t = applyEvent(t, ev({ type: "agent.future", agent_id: "a2" }, "a2"), 1);
    expect(t.live?.steps).toEqual([]);
  });

  it("records a denied call, warnings, a cancelled run and its unanswered calls", () => {
    let t = run();
    const send = (e: Event) => (t = applyEvent(t, ev(e, "a2"), 3000));
    send({
      type: "agent.warning",
      agent_id: "a2",
      kind: "tools_changed",
      message: "the tool set changed",
    });
    send({
      type: "agent.tool_call",
      agent_id: "a2",
      call_id: "c3",
      name: "write_file",
      input: { path: "x" },
    });
    send({
      type: "permission.decision",
      agent_id: "a2",
      call_id: "c3",
      tool: "write_file",
      decision: "deny",
      source: "user",
      reason: "no",
    });
    send({
      type: "agent.tool_result",
      agent_id: "a2",
      call_id: "c3",
      name: "write_file",
      ok: false,
      summary: "write_file: denied",
    });
    expect(t.calls.c3?.status).toBe("denied");
    expect(t.calls.c3?.permission).toEqual({ decision: "deny", source: "user", reason: "no" });
    send({
      type: "agent.tool_call",
      agent_id: "a2",
      call_id: "c4",
      name: "shell",
      input: { command: "sleep 100" },
    });
    t = cancellingLive(t);
    expect(t.live?.phase).toBe("cancelling");
    send({ type: "agent.finished", agent_id: "a2", status: "cancelled" });
    expect(t.calls.c4?.status).toBe("error");
    expect(t.agents.a2?.status).toBe("cancelled");
    expect(t.agents.a2?.notes.map((n) => n.kind)).toEqual(["warning"]);
  });

  it("keeps a failed agent.run as an error item that can retry the prompt", () => {
    let t = startLive(emptyTranscript("s1"), "hello", 0);
    t = failLive(t, {
      code: -32603,
      message: "connection to daemon closed",
      data: { kind: "daemon_unavailable" },
    });
    expect(isBusy(t)).toBe(false);
    const items = deriveItems(t);
    expect(kinds(items)).toEqual(["user", "error"]);
    const err = items[1];
    expect(err?.kind === "error" && err.retry).toBe("hello");
    expect(clearLive(t).live).toBeDefined();
  });

  it("keeps a run's finished status when a refresh still says running", () => {
    let t = run();
    t = applyEvent(
      t,
      ev(
        {
          type: "agent.finished",
          agent_id: "a2",
          status: "error",
          error: { code: 1, message: "boom" },
        },
        "a2",
      ),
      5,
    );
    t = mergeStored(
      t,
      page([], {
        agents: [
          {
            id: "a2",
            status: "running",
            started_at: "2026-09-12T10:00:05.000Z",
            calls: 0,
            usage: {
              input_tokens: 0,
              output_tokens: 0,
              cache_read_input_tokens: 0,
              cache_creation_input_tokens: 0,
            },
          },
        ],
      }),
      "refresh",
    );
    expect(t.agents.a2?.status).toBe("error");
    expect(t.agents.a2?.error?.message).toBe("boom");
  });
});
