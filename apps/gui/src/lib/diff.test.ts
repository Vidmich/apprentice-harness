import { describe, expect, it } from "vitest";
import { extractDiff, parseDiff, sideBySide } from "./diff";

const RESULT = `edited src/x.rs (+2 -1, 1 replacement)

--- a/src/x.rs
+++ b/src/x.rs
@@ -1,3 +1,4 @@
 fn main() {
-    println!("hi");
+    println!("hello");
+    println!("world");
 }
`;

describe("unified diffs", () => {
  it("are found after the result header and parsed into numbered lines", () => {
    const diff = extractDiff(RESULT);
    expect(diff?.startsWith("--- a/src/x.rs\n+++ b/src/x.rs\n@@")).toBe(true);
    const parsed = parseDiff(diff!);
    expect(parsed.oldPath).toBe("a/src/x.rs");
    expect(parsed.newPath).toBe("b/src/x.rs");
    expect(parsed.added).toBe(2);
    expect(parsed.removed).toBe(1);
    expect(parsed.hunks).toHaveLength(1);
    const lines = parsed.hunks[0]!.lines;
    expect(lines.map((l) => [l.kind, l.oldNo, l.newNo])).toEqual([
      ["context", 1, 1],
      ["del", 2, undefined],
      ["add", undefined, 2],
      ["add", undefined, 3],
      ["context", 3, 4],
    ]);
    expect(lines[2]?.text).toBe('    println!("hello");');
  });

  it("pairs deletions with additions side by side", () => {
    const rows = sideBySide(parseDiff(extractDiff(RESULT)!).hunks[0]!.lines);
    expect(rows.map((r) => [r.left?.kind, r.right?.kind])).toEqual([
      ["context", "context"],
      ["del", "add"],
      [undefined, "add"],
      ["context", "context"],
    ]);
  });

  it("finds no diff in a plain result or a failed edit", () => {
    expect(extractDiff("wrote src/new.rs (12 bytes, new file, 1 line)\n")).toBeUndefined();
    expect(
      extractDiff("edit_file failed: `old_string` not found; closest line 4: --- x"),
    ).toBeUndefined();
    expect(extractDiff("--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n")).toBeDefined();
  });
});
