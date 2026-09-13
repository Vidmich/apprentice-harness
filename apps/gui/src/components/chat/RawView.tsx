import { useVirtualizer } from "@tanstack/react-virtual";
import { useMemo, useRef } from "react";
import { bytes } from "../../lib/format";
import CopyButton from "./CopyButton";

const LINE_PX = 18;
/** Lines beyond this are cut in the view (the trace keeps the blob). */
const MAX_LINE_CHARS = 4000;

/**
 * A large text (a tool's raw output) as a virtualised list of lines:
 * megabytes render as a few dozen rows.
 */
export default function RawView({ text, note }: { text: string; note?: string }) {
  const lines = useMemo(() => text.split("\n"), [text]);
  const parent = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: lines.length,
    getScrollElement: () => parent.current,
    estimateSize: () => LINE_PX,
    overscan: 20,
  });
  return (
    <div>
      <div className="mb-1 flex items-center gap-2 text-xs text-muted">
        <span>
          {lines.length.toLocaleString("en-US")} lines · {bytes(text.length)}
        </span>
        {note !== undefined && <span>· {note}</span>}
        <span className="grow" />
        <CopyButton text={text} label="copy all" />
      </div>
      <div
        ref={parent}
        className="max-h-96 overflow-auto rounded border border-border bg-panel font-mono text-xs"
        tabIndex={0}
      >
        <div style={{ height: virtualizer.getTotalSize(), position: "relative", minWidth: "100%" }}>
          {virtualizer.getVirtualItems().map((v) => {
            const line = lines[v.index] ?? "";
            return (
              <div
                key={v.key}
                className="absolute top-0 left-0 whitespace-pre px-2"
                style={{
                  height: LINE_PX,
                  lineHeight: `${LINE_PX}px`,
                  transform: `translateY(${v.start}px)`,
                }}
              >
                {line.length > MAX_LINE_CHARS ? `${line.slice(0, MAX_LINE_CHARS)} …` : line}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
