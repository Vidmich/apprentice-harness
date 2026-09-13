import { describe, expect, it } from "vitest";
import { ago, compactNum, dayGroup, sessionTotals, uptime } from "./format";

describe("sidebar times", () => {
  it("says how long ago, coarsely", () => {
    const now = Date.parse("2026-09-13T12:00:00Z");
    expect(ago("2026-09-13T11:59:40Z", now)).toBe("now");
    expect(ago("2026-09-13T11:56:00Z", now)).toBe("4m");
    expect(ago("2026-09-13T09:30:00Z", now)).toBe("2h");
    expect(ago("2026-09-11T12:00:00Z", now)).toBe("2d");
    expect(ago("2026-08-01T12:00:00Z", now)).toMatch(/Aug/);
    expect(ago("nonsense", now)).toBe("nonsense");
  });

  it("groups by local day", () => {
    const now = new Date(2026, 8, 13, 12, 0, 0);
    expect(dayGroup(new Date(2026, 8, 13, 0, 30).toISOString(), now)).toBe("Today");
    expect(dayGroup(new Date(2026, 8, 12, 23, 30).toISOString(), now)).toBe("Yesterday");
    expect(dayGroup(new Date(2026, 8, 11, 12, 0).toISOString(), now)).toBe("Earlier");
    expect(dayGroup("nonsense", now)).toBe("Earlier");
  });

  it("compacts the session header's counts", () => {
    expect(compactNum(950)).toBe("950");
    expect(compactNum(1000)).toBe("1k");
    expect(compactNum(182_340)).toBe("182k");
    expect(compactNum(1_200_000)).toBe("1.2M");
    expect(compactNum(3_000_000)).toBe("3M");
    const usage = {
      input_tokens: 182_340,
      output_tokens: 21_004,
      cache_read_input_tokens: 610_222,
      cache_creation_input_tokens: 12_000,
    };
    expect(sessionTotals(usage, 1.84, 14)).toBe(
      "in 182k · out 21k · cached 610k · $1.84 · 14 calls",
    );
    expect(sessionTotals(usage, undefined, 1)).toBe("in 182k · out 21k · cached 610k · 1 call");
  });

  it("formats an uptime", () => {
    expect(uptime(42)).toBe("42s");
    expect(uptime(600)).toBe("10m");
    expect(uptime(3720)).toBe("1h 02m");
    expect(uptime(3 * 86_400)).toBe("3d 0h");
  });
});
