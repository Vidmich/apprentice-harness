import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("./bridge", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));

const { RpcFailure, call, describe: describeError, stream } = await import("./rpc");

describe("rpc bridge", () => {
  beforeEach(() => invoke.mockReset());

  it("passes method and params through rpc_call and returns the result", async () => {
    invoke.mockResolvedValueOnce({ session_id: "s1" });
    const r = await call("session.create", { title: "t" });
    expect(r.session_id).toBe("s1");
    expect(invoke).toHaveBeenCalledWith("rpc_call", {
      method: "session.create",
      params: { title: "t" },
    });
  });

  it("uses rpc_stream with the caller's channel for streaming methods", async () => {
    invoke.mockResolvedValueOnce({
      subscription: "agent-1",
      result: { agent_id: "agent-1", subscription: "agent-1" },
    });
    const started = await stream("agent.run", { session_id: "s", prompt: "hi" }, "ch-1");
    expect(started.subscription).toBe("agent-1");
    expect(started.result.agent_id).toBe("agent-1");
    expect(invoke).toHaveBeenCalledWith("rpc_stream", {
      method: "agent.run",
      params: { session_id: "s", prompt: "hi" },
      channel: "ch-1",
    });
  });

  it("throws an RpcFailure carrying the daemon's error", async () => {
    invoke.mockRejectedValueOnce({
      code: -32010,
      message: "no such session",
      data: { kind: "not_found" },
    });
    const err = await call("agent.run", { session_id: "x", prompt: "y" }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(RpcFailure);
    const failure = err as InstanceType<typeof RpcFailure>;
    expect(failure.kind).toBe("not_found");
    expect(failure.error.code).toBe(-32010);
    expect(failure.message).toBe("no such session [not_found]");
  });

  it("wraps a non-RPC rejection as a bridge error", async () => {
    invoke.mockRejectedValueOnce(new Error("command not found"));
    const err = (await call("auth.status", {}).catch((e: unknown) => e)) as InstanceType<
      typeof RpcFailure
    >;
    expect(err.kind).toBe("bridge");
    expect(err.message).toBe("command not found [bridge]");
  });

  it("describes errors like the CLI does", () => {
    expect(describeError({ code: 1, message: "m" })).toBe("m");
    expect(describeError({ code: 1, message: "m", data: { kind: "k" } })).toBe("m [k]");
  });
});
