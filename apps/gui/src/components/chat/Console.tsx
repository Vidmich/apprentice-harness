import { useEffect, useMemo, useRef, useState } from "react";
import { bytes } from "../../lib/format";
import type { ProgressChunk } from "../../lib/transcript";

/** Lines the console shows; the Raw tab has everything. */
const TAIL_LINES = 300;

/** The streamed output of a running shell: autoscroll with a follow toggle. */
export default function Console({ progress, live }: { progress: ProgressChunk[]; live: boolean }) {
  const [follow, setFollow] = useState(true);
  const el = useRef<HTMLPreElement>(null);
  const { text, dropped, total } = useMemo(() => {
    let size = 0;
    for (const p of progress) size += p.text.length;
    const all = progress.map((p) => p.text).join("");
    const lines = all.split("\n");
    const dropped = Math.max(0, lines.length - TAIL_LINES);
    return { text: lines.slice(dropped).join("\n"), dropped, total: size };
  }, [progress]);

  useEffect(() => {
    if (follow && el.current) el.current.scrollTop = el.current.scrollHeight;
  }, [text, follow]);

  return (
    <div>
      <div className="mb-1 flex items-center gap-2 text-xs text-muted">
        <span>
          {bytes(total)} streamed{dropped > 0 ? ` · first ${dropped} lines not shown` : ""}
          {live ? " · running" : ""}
        </span>
        <span className="grow" />
        <label className="flex items-center gap-1">
          <input type="checkbox" checked={follow} onChange={(e) => setFollow(e.target.checked)} />
          follow
        </label>
      </div>
      <pre
        ref={el}
        className="max-h-72 overflow-auto rounded border border-border bg-panel p-2 font-mono text-xs whitespace-pre-wrap"
        onScroll={(e) => {
          const t = e.currentTarget;
          const atEnd = t.scrollHeight - t.scrollTop - t.clientHeight < 8;
          if (!atEnd && follow) setFollow(false);
        }}
      >
        {text}
      </pre>
    </div>
  );
}
