// Builds `harnessd` in release mode and copies it to
// `src-tauri/binaries/harnessd-<target-triple>[.exe]`, the name Tauri 2
// expects for a sidecar. Run before `tauri build` (see `build:app`).

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const gui = dirname(dirname(fileURLToPath(import.meta.url)));
const workspace = join(gui, "..", "..");
const exe = process.platform === "win32" ? ".exe" : "";

const triple = execFileSync("rustc", ["-vV"], { encoding: "utf8" })
  .split("\n")
  .find((l) => l.startsWith("host: "))
  ?.slice("host: ".length)
  .trim();
if (!triple) throw new Error("cannot read the host target triple from `rustc -vV`");

console.log("building harnessd (release)...");
execFileSync("cargo", ["build", "--release", "-p", "harnessd", "--bin", "harnessd"], {
  cwd: workspace,
  stdio: "inherit",
});

const from = join(workspace, "target", "release", `harnessd${exe}`);
const dir = join(gui, "src-tauri", "binaries");
const to = join(dir, `harnessd-${triple}${exe}`);
mkdirSync(dir, { recursive: true });
copyFileSync(from, to);
console.log(`sidecar: ${to}`);
