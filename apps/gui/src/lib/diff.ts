// The unified diff an `edit_file` / `write_file` result carries (after
// its `edited <path> (...)` header and a blank line), parsed for the
// Diff tab, and paired into rows for the side-by-side view.

export type LineKind = "context" | "add" | "del";

export interface DiffLine {
  kind: LineKind;
  text: string;
  /** Line numbers in the old and new file (absent for the missing side). */
  oldNo?: number;
  newNo?: number;
}

export interface Hunk {
  header: string;
  lines: DiffLine[];
}

export interface ParsedDiff {
  oldPath?: string;
  newPath?: string;
  hunks: Hunk[];
  added: number;
  removed: number;
}

const HUNK = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

/** The unified diff in a tool result's text, or `undefined` when it has none. */
export function extractDiff(text: string): string | undefined {
  const m = /(^|\n)(--- [^\n]*\n\+\+\+ [^\n]*\n@@ )/.exec(text);
  if (m === null) return undefined;
  return text.slice(m.index + m[1]!.length);
}

/** Parses a unified diff (one file). Lines outside hunks other than the headers are ignored. */
export function parseDiff(diff: string): ParsedDiff {
  const out: ParsedDiff = { hunks: [], added: 0, removed: 0 };
  let hunk: Hunk | undefined;
  let oldNo = 0;
  let newNo = 0;
  for (const raw of diff.replace(/\n$/, "").split("\n")) {
    if (hunk === undefined && raw.startsWith("--- ")) {
      out.oldPath = raw.slice(4);
      continue;
    }
    if (hunk === undefined && raw.startsWith("+++ ")) {
      out.newPath = raw.slice(4);
      continue;
    }
    const m = HUNK.exec(raw);
    if (m !== null) {
      oldNo = Number(m[1]);
      newNo = Number(m[2]);
      hunk = { header: raw, lines: [] };
      out.hunks.push(hunk);
      continue;
    }
    if (hunk === undefined) continue;
    if (raw.startsWith("+")) {
      hunk.lines.push({ kind: "add", text: raw.slice(1), newNo: newNo++ });
      out.added++;
    } else if (raw.startsWith("-")) {
      hunk.lines.push({ kind: "del", text: raw.slice(1), oldNo: oldNo++ });
      out.removed++;
    } else if (raw.startsWith("\\")) {
      // "\ No newline at end of file"
      continue;
    } else {
      hunk.lines.push({ kind: "context", text: raw.slice(1), oldNo: oldNo++, newNo: newNo++ });
    }
  }
  return out;
}

export interface SideRow {
  left?: DiffLine;
  right?: DiffLine;
}

/**
 * Pairs a hunk's lines for the side-by-side view: a run of deletions
 * followed by a run of additions lines up row by row; context sits on
 * both sides.
 */
export function sideBySide(lines: DiffLine[]): SideRow[] {
  const rows: SideRow[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i]!;
    if (line.kind === "context") {
      rows.push({ left: line, right: line });
      i++;
      continue;
    }
    const dels: DiffLine[] = [];
    const adds: DiffLine[] = [];
    while (i < lines.length && lines[i]!.kind === "del") dels.push(lines[i++]!);
    while (i < lines.length && lines[i]!.kind === "add") adds.push(lines[i++]!);
    const n = Math.max(dels.length, adds.length);
    for (let k = 0; k < n; k++) {
      const row: SideRow = {};
      const left = dels[k];
      const right = adds[k];
      if (left !== undefined) row.left = left;
      if (right !== undefined) row.right = right;
      rows.push(row);
    }
  }
  return rows;
}
