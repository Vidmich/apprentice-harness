import { describe, expect, it } from "vitest";
import type { CallSummary } from "./api";
import {
  CALL_CSV_COLUMNS,
  callsCsv,
  exportName,
  localDate,
  prettyJson,
  rangeLabel,
  rangeParams,
  tokensOf,
} from "./usage";

type Over = { [K in keyof CallSummary]?: CallSummary[K] | undefined };

function call(over: Over = {}): CallSummary {
  const base: Record<string, unknown> = {
    call_id: "m1",
    started_at: "2026-09-12T10:04:00.000Z",
    session_id: "s1",
    session_title: "fix tests",
    workspace_id: "w1",
    agent_id: "a1",
    kind: "step",
    model: "claude-opus-5",
    effort: "high",
    status: "ok",
    stop_reason: "end_turn",
    input: 1204,
    output: 310,
    cache_read: 0,
    cache_creation: 900,
    cost_usd: 0.0138,
    total_ms: 4200,
    request_event_id: "e7",
    ...over,
  };
  // `undefined` in `over` means absent.
  for (const k of Object.keys(base)) if (base[k] === undefined) delete base[k];
  return base as unknown as CallSummary;
}

describe("usage ranges", () => {
  it("turns the presets into what the daemon parses", () => {
    const now = new Date(2026, 8, 13, 9, 30);
    expect(localDate(now)).toBe("2026-09-13");
    expect(rangeParams({ preset: "today", since: "", until: "" }, now)).toEqual({
      since: "2026-09-13",
    });
    expect(rangeParams({ preset: "7d", since: "", until: "" })).toEqual({ since: "7d" });
    expect(rangeParams({ preset: "30d", since: "", until: "" })).toEqual({ since: "30d" });
    expect(rangeParams({ preset: "custom", since: "2026-09-01", until: "" })).toEqual({
      since: "2026-09-01",
    });
    expect(rangeParams({ preset: "custom", since: "", until: "2026-09-10" })).toEqual({
      until: "2026-09-10",
    });
    expect(rangeLabel({ preset: "custom", since: "", until: "2026-09-10" })).toBe(
      "the beginning → 2026-09-10",
    );
    expect(rangeLabel({ preset: "7d", since: "", until: "" })).toBe("last 7 days");
    expect(exportName({ preset: "7d", since: "", until: "" }, now)).toBe(
      "mentor-calls-7d-2026-09-13.csv",
    );
    expect(exportName({ preset: "custom", since: "2026-09-01", until: "" }, now)).toBe(
      "mentor-calls-2026-09-01_now-2026-09-13.csv",
    );
  });

  it("counts every token of a bucket", () => {
    expect(
      tokensOf({
        calls: 1,
        input: 10,
        output: 20,
        cache_read: 300,
        cache_creation: 4,
        cost_usd: 0,
        unpriced_calls: 0,
      }),
    ).toBe(334);
  });
});

describe("calls export", () => {
  it("writes the CLI's columns, quoting what needs it", () => {
    const text = callsCsv([
      call(),
      call({
        call_id: "m2",
        session_title: 'say "hi", twice',
        effort: undefined,
        stop_reason: undefined,
        cost_usd: undefined,
        total_ms: undefined,
        status: "error",
      }),
    ]);
    const lines = text.split("\r\n");
    expect(lines[0]).toBe(CALL_CSV_COLUMNS.join(","));
    expect(lines[1]).toBe(
      "m1,2026-09-12T10:04:00.000Z,s1,fix tests,w1,a1,step,claude-opus-5,high,ok,end_turn,1204,310,0,900,0.0138,4200,e7",
    );
    expect(lines[2]).toBe(
      'm2,2026-09-12T10:04:00.000Z,s1,"say ""hi"", twice",w1,a1,step,claude-opus-5,,error,,1204,310,0,900,,,e7',
    );
    expect(lines[3]).toBe("");
    expect(CALL_CSV_COLUMNS.length).toBe(18);
  });

  it("pretty-prints a stored body, leaving non-JSON alone", () => {
    expect(prettyJson('{"model":"m","max_tokens":1}')).toBe(
      '{\n  "model": "m",\n  "max_tokens": 1\n}',
    );
    expect(prettyJson("not json")).toBe("not json");
  });
});
