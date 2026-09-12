import { describe, expect, it } from "vitest";
import { appVersion } from "./rpc";

describe("rpc placeholder", () => {
  it("exposes the app version", () => {
    expect(appVersion).toMatch(/^\d+\.\d+\.\d+$/);
  });
});
