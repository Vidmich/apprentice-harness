import { describe, expect, it } from "vitest";
import type { Event, EventNotification, Usage } from "./api";
import {
  applyEvent,
  attachRun,
  cancellingRun,
  failRun,
  idleRun,
  runUsageLine,
  startingRun,
  usageLine,
} from "./run";

function ev(subscription: string, seq: number, event: Event): EventNotification {
  return { subscription, seq, event };
}

const delta = (agent: string, text: string): Event => ({
  type: "agent.text_delta",
  agent_id: agent,
  text,
});

const usage = (agent: string, call: string, u: Partial<Usage>, cost?: number): Event => ({
  type: "agent.usage",
  agent_id: agent,
  call_id: call,
  usage: {
    input_tokens: 0,
    output_tokens: 0,
    cache_read_input_tokens: 0,
    cache_creation_input_tokens: 0,
    ...u,
  },
  ...(cost === undefined ? {} : { cost_usd: cost }),
});

const finished = (agent: string, status: "ok" | "cancelled" | "error"): Event => ({
  type: "agent.finished",
  agent_id: agent,
  status,
});

describe("run reducer", () => {
  it("streams text, sums usage and cost, and finishes", () => {
    let run = attachRun(startingRun("s1", 1000), "a1", "a1");
    const started: Event = { type: "agent.started", agent_id: "a1", session_id: "s1" };
    run = applyEvent(run, ev("a1", 1, started), 1001);
    run = applyEvent(run, ev("a1", 2, delta("a1", "Hello, ")), 1002);
    run = applyEvent(run, ev("a1", 3, delta("a1", "world!")), 1003);
    run = applyEvent(
      run,
      ev("a1", 4, usage("a1", "c1", { input_tokens: 1000, output_tokens: 300 }, 0.01)),
      1004,
    );
    run = applyEvent(
      run,
      ev("a1", 5, usage("a1", "c2", { input_tokens: 204, output_tokens: 10 }, 0.0038)),
      1005,
    );
    expect(run.phase).toBe("running");
    expect(run.text).toBe("Hello, world!");
    run = applyEvent(run, ev("a1", 6, finished("a1", "ok")), 5210);
    expect(run.phase).toBe("done");
    expect(run.status).toBe("ok");
    expect(run.truncated).toBeUndefined();
    expect(run.lastSeq).toBe(6);
    expect(runUsageLine(run, 99_999)).toBe("↳ in 1,204 · out 310 · cache read 0 · $0.0138 · 4.2s");
    // Nothing after the terminal event changes the run.
    expect(applyEvent(run, ev("a1", 7, delta("a1", "late")), 6000)).toBe(run);
  });

  it("keeps the truncation flag of a finished run", () => {
    let run = attachRun(startingRun("s1", 0), "a1", "a1");
    const cut: Event = { type: "agent.finished", agent_id: "a1", status: "ok", truncated: true };
    run = applyEvent(run, ev("a1", 1, cut), 1);
    expect(run.status).toBe("ok");
    expect(run.truncated).toBe(true);
  });

  it("omits the cost when no call reported one", () => {
    let run = attachRun(startingRun("s1", 0), "a1", "a1");
    run = applyEvent(
      run,
      ev("a1", 1, usage("a1", "c1", { input_tokens: 5, cache_creation_input_tokens: 9 })),
      1,
    );
    expect(run.costUsd).toBeUndefined();
    expect(runUsageLine(run, 40)).toBe("↳ in 5 · out 0 · cache read 0 · cache write 9 · 0.0s");
  });

  it("keeps two runs apart: events for another subscription are ignored", () => {
    let a = attachRun(startingRun("s", 0), "a", "a");
    let b = attachRun(startingRun("s", 0), "b", "b");
    const interleaved = [
      ev("a", 1, delta("a", "A1")),
      ev("b", 1, delta("b", "B1")),
      ev("a", 2, delta("a", "A2")),
      ev("b", 2, finished("b", "cancelled")),
      ev("a", 3, finished("a", "ok")),
    ];
    for (const n of interleaved) {
      a = applyEvent(a, n, 10);
      b = applyEvent(b, n, 10);
    }
    expect(a.text).toBe("A1A2");
    expect(b.text).toBe("B1");
    expect(a.status).toBe("ok");
    expect(b.status).toBe("cancelled");
  });

  it("adopts the subscription from the first event when the answer is late", () => {
    let run = startingRun("s", 0);
    run = applyEvent(run, ev("agent-9", 1, delta("agent-9", "early")), 1);
    expect(run.phase).toBe("running");
    expect(run.subscription).toBe("agent-9");
    expect(run.text).toBe("early");
    run = applyEvent(run, ev("agent-9", 2, finished("agent-9", "ok")), 2);
    // The late answer must not resurrect a finished run.
    run = attachRun(run, "agent-9", "agent-9");
    expect(run.phase).toBe("done");
    expect(run.agentId).toBe("agent-9");
  });

  it("records cancellation, errors and activity", () => {
    let run = attachRun(startingRun("s", 0), "a", "a");
    run = cancellingRun(run);
    expect(run.phase).toBe("cancelling");
    const call: Event = {
      type: "agent.tool_call",
      agent_id: "a",
      call_id: "c",
      name: "read_file",
      input: { path: "x" },
    };
    const result: Event = {
      type: "agent.tool_result",
      agent_id: "a",
      call_id: "c",
      ok: true,
      summary: "12 lines",
    };
    run = applyEvent(run, ev("a", 1, call), 1);
    run = applyEvent(run, ev("a", 2, result), 2);
    run = applyEvent(run, ev("a", 3, { type: "log", level: "warn", message: "slow" }), 3);
    run = applyEvent(run, ev("a", 4, { type: "agent.future_thing", x: 1 }), 4);
    expect(run.activity).toEqual(['→ read_file {"path":"x"}', "← ok 12 lines", "warn: slow"]);
    expect(run.lastSeq).toBe(4);
    const cancelled: Event = {
      type: "agent.finished",
      agent_id: "a",
      status: "cancelled",
      error: { code: -32030, message: "operation cancelled", data: { kind: "cancelled" } },
    };
    run = applyEvent(run, ev("a", 5, cancelled), 5);
    expect(run.status).toBe("cancelled");
    expect(run.error?.data?.kind).toBe("cancelled");

    const failed = failRun(startingRun("s", 0), { code: -32603, message: "boom" }, 1);
    expect(failed.phase).toBe("done");
    expect(failed.status).toBe("error");
    expect(applyEvent(idleRun(), ev("a", 1, delta("a", "x")), 1).text).toBe("");
  });

  it("renders the usage line like the CLI", () => {
    const u: Usage = {
      input_tokens: 1204,
      output_tokens: 310,
      cache_read_input_tokens: 0,
      cache_creation_input_tokens: 0,
    };
    expect(usageLine(u, 0.0138, 4.21)).toBe("↳ in 1,204 · out 310 · cache read 0 · $0.0138 · 4.2s");
    expect(usageLine({ ...u, cache_creation_input_tokens: 900 }, undefined, 0.04)).toBe(
      "↳ in 1,204 · out 310 · cache read 0 · cache write 900 · 0.0s",
    );
    expect(usageLine(u, 12.5, 1)).toBe("↳ in 1,204 · out 310 · cache read 0 · $12.50 · 1.0s");
  });
});
