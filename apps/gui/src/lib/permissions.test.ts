import { describe, expect, it } from "vitest";
import type { Event } from "./api";
import {
  EMPTY_PERMISSIONS,
  applyPermissionEvent,
  countBySession,
  countdown,
  expired,
  pendingFor,
  secondsLeft,
  withoutRequest,
} from "./permissions";

function request(id: string, agent: string, extra: Partial<Record<string, unknown>> = {}): Event {
  return {
    type: "permission.request",
    request_id: id,
    agent_id: agent,
    tool: "shell",
    input: { command: "cargo test" },
    risk: "execute",
    description: "shell: cargo test",
    command: "cargo test",
    suggested_rules: [{ tool: "shell", effect: "allow", match: { command_prefix: "cargo" } }],
    timeout_s: 600,
    ...extra,
  } as Event;
}

function decision(agent: string, requestId?: string): Event {
  const e: Record<string, unknown> = {
    type: "permission.decision",
    agent_id: agent,
    call_id: "c1",
    tool: "shell",
    decision: "allow",
    source: "user",
  };
  if (requestId !== undefined) e.request_id = requestId;
  return e as unknown as Event;
}

describe("permission routing", () => {
  it("keeps each session's requests apart, in arrival order", () => {
    let s = applyPermissionEvent(EMPTY_PERMISSIONS, "s1", request("p1", "a1"), 1000);
    s = applyPermissionEvent(s, "s2", request("p2", "a2"), 1001);
    s = applyPermissionEvent(s, "s1", request("p3", "a1", { tool: "write_file" }), 1002);
    expect(pendingFor(s, "s1").map((p) => p.requestId)).toEqual(["p1", "p3"]);
    expect(pendingFor(s, "s2").map((p) => p.requestId)).toEqual(["p2"]);
    expect(pendingFor(s, "s3")).toEqual([]);
    expect(countBySession(s)).toEqual({ s1: 2, s2: 1 });
    const p1 = pendingFor(s, "s1")[0]!;
    expect(p1.command).toBe("cargo test");
    expect(p1.suggestedRules[0]?.match?.command_prefix).toBe("cargo");
    expect(p1.receivedAt).toBe(1000);
    // The same request twice (a reattached subscription replays nothing, but be safe).
    expect(applyPermissionEvent(s, "s1", request("p1", "a1"), 2000)).toBe(s);
  });

  it("closes a request on the decision that names it, and all of an agent's at its end", () => {
    let s = applyPermissionEvent(EMPTY_PERMISSIONS, "s1", request("p1", "a1"), 0);
    s = applyPermissionEvent(s, "s1", request("p2", "a1"), 0);
    s = applyPermissionEvent(s, "s2", request("p3", "a2"), 0);
    // A decision without a request id (a rule decided) changes nothing.
    expect(applyPermissionEvent(s, "s1", decision("a1"), 0)).toBe(s);
    s = applyPermissionEvent(s, "s1", decision("a1", "p1"), 0);
    expect(pendingFor(s, "s1").map((p) => p.requestId)).toEqual(["p2"]);
    s = applyPermissionEvent(
      s,
      "s1",
      { type: "agent.finished", agent_id: "a1", status: "cancelled" } as Event,
      0,
    );
    expect(pendingFor(s, "s1")).toEqual([]);
    expect(pendingFor(s, "s2").map((p) => p.requestId)).toEqual(["p3"]);
    expect(withoutRequest(s, "nope")).toBe(s);
    expect(withoutRequest(s, "p3").order).toEqual([]);
  });

  it("counts down to the daemon's timeout and expires", () => {
    const s = applyPermissionEvent(EMPTY_PERMISSIONS, "s1", request("p1", "a1"), 10_000);
    const p = s.pending.p1!;
    expect(secondsLeft(p, 10_000)).toBe(600);
    expect(secondsLeft(p, 10_000 + 2_500)).toBe(598);
    expect(secondsLeft(p, 10_000 + 600_000)).toBe(0);
    expect(secondsLeft(p, 10_000 + 700_000)).toBe(0);
    expect(countdown(598)).toBe("9:58");
    expect(countdown(5)).toBe("0:05");
    expect(expired(s, 10_000 + 599_000)).toBe(s);
    expect(expired(s, 10_000 + 600_000).order).toEqual([]);
    const forever = applyPermissionEvent(
      EMPTY_PERMISSIONS,
      "s1",
      request("p2", "a1", { timeout_s: 0 }),
      0,
    );
    expect(secondsLeft(forever.pending.p2!, 1e12)).toBe(Number.POSITIVE_INFINITY);
    expect(countdown(Number.POSITIVE_INFINITY)).toBe("");
  });
});
