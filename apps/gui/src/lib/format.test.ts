import { describe, expect, it } from "vitest";
import { ago, dayGroup, uptime } from "./format";

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

  it("formats an uptime", () => {
    expect(uptime(42)).toBe("42s");
    expect(uptime(600)).toBe("10m");
    expect(uptime(3720)).toBe("1h 02m");
    expect(uptime(3 * 86_400)).toBe("3d 0h");
  });
});
