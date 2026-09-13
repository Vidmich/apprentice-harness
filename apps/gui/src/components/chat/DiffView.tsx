import { useMemo, useState } from "react";
import { type DiffLine, parseDiff, sideBySide } from "../../lib/diff";

const KIND_CLASS: Record<DiffLine["kind"], string> = {
  context: "",
  add: "bg-ok/15",
  del: "bg-bad/15",
};

const MARK: Record<DiffLine["kind"], string> = { context: " ", add: "+", del: "-" };

function Cell({ line, no }: { line: DiffLine | undefined; no: number | undefined }) {
  return (
    <>
      <td className="w-10 select-none pr-2 text-right text-muted">{no ?? ""}</td>
      <td className={`w-4 select-none ${line === undefined ? "" : KIND_CLASS[line.kind]}`}>
        {line === undefined ? "" : MARK[line.kind]}
      </td>
      <td
        className={`whitespace-pre ${line === undefined ? "bg-panel/60" : KIND_CLASS[line.kind]}`}
      >
        {line?.text ?? ""}
      </td>
    </>
  );
}

/** A unified diff, as hunks; unified or side by side. */
export default function DiffView({ diff }: { diff: string }) {
  const parsed = useMemo(() => parseDiff(diff), [diff]);
  const [split, setSplit] = useState(false);
  return (
    <div className="text-xs">
      <div className="mb-1 flex items-center gap-3 text-muted">
        <span className="font-mono">{parsed.newPath ?? parsed.oldPath ?? ""}</span>
        <span>
          <span className="text-ok">+{parsed.added}</span>{" "}
          <span className="text-bad">−{parsed.removed}</span>
        </span>
        <span className="grow" />
        <label className="flex items-center gap-1">
          <input type="checkbox" checked={split} onChange={(e) => setSplit(e.target.checked)} />
          side by side
        </label>
      </div>
      <div className="overflow-x-auto rounded border border-border">
        <table className="w-full border-collapse font-mono leading-5">
          <tbody>
            {parsed.hunks.map((hunk, h) => (
              <HunkRows key={h} header={hunk.header} lines={hunk.lines} split={split} />
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function HunkRows({ header, lines, split }: { header: string; lines: DiffLine[]; split: boolean }) {
  const rows = useMemo(() => (split ? sideBySide(lines) : undefined), [lines, split]);
  return (
    <>
      <tr>
        <td colSpan={split ? 6 : 3} className="bg-panel px-2 text-muted">
          {header}
        </td>
      </tr>
      {rows !== undefined
        ? rows.map((row, i) => (
            <tr key={i}>
              <Cell line={row.left} no={row.left?.oldNo} />
              <Cell line={row.right} no={row.right?.newNo} />
            </tr>
          ))
        : lines.map((line, i) => (
            <tr key={i} className={KIND_CLASS[line.kind]}>
              <td className="w-10 select-none pr-2 text-right text-muted">{line.oldNo ?? ""}</td>
              <td className="w-10 select-none pr-2 text-right text-muted">{line.newNo ?? ""}</td>
              <td className="whitespace-pre">
                <span className="select-none">{MARK[line.kind]}</span>
                {line.text}
              </td>
            </tr>
          ))}
    </>
  );
}
