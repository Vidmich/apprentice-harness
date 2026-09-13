import { memo, useEffect, useId, useMemo, useState } from "react";
import { type RawOutput, rawOutput } from "../../lib/chat";
import { extractDiff } from "../../lib/diff";
import { bytes, compact, duration } from "../../lib/format";
import type { CallMeta, CallStatus, Item } from "../../lib/transcript";
import Console from "./Console";
import CopyButton from "./CopyButton";
import DiffView from "./DiffView";
import RawView from "./RawView";

type Card = Extract<Item, { kind: "tool_call" }>;
type Tab = "input" | "result" | "raw" | "diff" | "console";

const ICONS: Record<string, string> = {
  read_file: "📄",
  write_file: "✏️",
  edit_file: "✏️",
  list_files: "📁",
  glob: "🔍",
  grep: "🔍",
  shell: "＄",
};

function icon(name: string): string {
  return ICONS[name] ?? (name.startsWith("git") ? "⎇" : "🔧");
}

const STATUS_LABEL: Record<CallStatus, string> = {
  running: "running",
  ok: "ok",
  error: "error",
  denied: "denied",
};

const STATUS_CLASS: Record<CallStatus, string> = {
  running: "text-accent",
  ok: "text-ok",
  error: "text-bad",
  denied: "text-warn",
};

/** A one-line description of the call from its input (the summary comes with the result). */
export function describeInput(input: unknown): string {
  if (typeof input === "object" && input !== null) {
    const o = input as Record<string, unknown>;
    for (const key of ["command", "path", "pattern", "query"]) {
      if (typeof o[key] === "string") return o[key];
    }
  }
  return input === undefined ? "" : compact(input, 80);
}

const MARKER = /(\[\.\.\. \d+ bytes omitted(?:, full result id [^\]]+)?\])/;

/** The result text with the daemon's truncation marker highlighted. */
function ResultText({ text }: { text: string }) {
  const parts = text.split(MARKER);
  return (
    <pre className="max-h-96 overflow-auto rounded border border-border bg-panel p-2 font-mono text-xs whitespace-pre-wrap">
      {parts.map((p, i) =>
        MARKER.test(p) ? (
          <mark
            key={i}
            className="rounded bg-warn/30 px-1 text-fg"
            title="Cut here for the mentor; the Raw tab has everything"
          >
            {p}
          </mark>
        ) : (
          p
        ),
      )}
    </pre>
  );
}

function statusOf(item: Card): CallStatus {
  if (item.meta !== undefined) return item.meta.status;
  if (item.result !== undefined) return item.result.isError ? "error" : "ok";
  return item.live ? "running" : "error";
}

/**
 * One tool call: header with status, then tabs — the input, the result
 * as the mentor read it, the raw output from the trace (lazy), the diff
 * of an edit, the console of a shell.
 */
function ToolCard({ item, sessionId }: { item: Card; sessionId: string }) {
  const { meta } = item;
  const status = statusOf(item);
  const isShell = item.name === "shell";
  const progress = meta?.progress ?? [];
  const diff = useMemo(
    () =>
      (item.name === "edit_file" || item.name === "write_file") && item.result !== undefined
        ? extractDiff(item.result.text)
        : undefined,
    [item.name, item.result],
  );
  const [openChoice, setOpen] = useState<boolean>();
  const open = openChoice ?? (status === "running" && progress.length > 0);
  const [tabChoice, setTab] = useState<Tab>();
  const tab: Tab =
    tabChoice ??
    (diff !== undefined
      ? "diff"
      : item.result !== undefined
        ? "result"
        : status === "running" && isShell
          ? "console"
          : "input");
  const baseId = useId();

  const tabs: Tab[] = ["input", "result", "raw"];
  if (diff !== undefined) tabs.push("diff");
  if (isShell && (progress.length > 0 || status === "running")) tabs.push("console");

  const summary = meta?.summary ?? describeInput(item.input);
  const elapsed =
    meta?.startedAt !== undefined && meta.endedAt !== undefined
      ? duration(meta.endedAt - meta.startedAt)
      : undefined;

  return (
    <section
      className="my-1.5 rounded border border-border"
      role="group"
      aria-label={`${item.name} tool call, ${STATUS_LABEL[status]}`}
    >
      <button
        type="button"
        className="flex w-full items-center gap-2 px-2 py-1 text-left text-sm hover:bg-panel"
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        aria-controls={`${baseId}-body`}
      >
        <span aria-hidden>{icon(item.name)}</span>
        <span className="font-mono">{item.name}</span>
        <span className="truncate text-muted" title={summary}>
          {summary}
        </span>
        <span className="grow" />
        {elapsed !== undefined && <span className="text-xs text-muted">{elapsed}</span>}
        <span className={`text-xs ${STATUS_CLASS[status]}`}>
          {status === "running" ? (
            <span className="animate-pulse">● running</span>
          ) : (
            STATUS_LABEL[status]
          )}
        </span>
        <span className="text-xs text-muted" aria-hidden>
          {open ? "▾" : "▸"}
        </span>
      </button>
      {open && (
        <div id={`${baseId}-body`} className="border-t border-border px-2 py-1.5">
          {meta?.permission !== undefined && (
            <p className="mb-1 text-xs text-warn">
              {meta.permission.decision === "deny" ? "denied" : "allowed"} by{" "}
              {meta.permission.source}
              {meta.permission.reason !== undefined ? `: ${meta.permission.reason}` : ""}
            </p>
          )}
          <div role="tablist" className="mb-1.5 flex gap-1 text-xs">
            {tabs.map((t) => (
              <button
                key={t}
                type="button"
                role="tab"
                id={`${baseId}-tab-${t}`}
                aria-selected={tab === t}
                aria-controls={`${baseId}-panel-${t}`}
                className={`rounded px-2 py-0.5 ${tab === t ? "bg-panel text-fg" : "text-muted hover:text-fg"}`}
                onClick={() => setTab(t)}
              >
                {t === "raw" ? "Raw" : t === "diff" ? "Diff" : t[0]!.toUpperCase() + t.slice(1)}
              </button>
            ))}
          </div>
          <div
            role="tabpanel"
            id={`${baseId}-panel-${tab}`}
            aria-labelledby={`${baseId}-tab-${tab}`}
          >
            {tab === "input" && <InputTab input={item.input} />}
            {tab === "result" && <ResultTab item={item} status={status} meta={meta} />}
            {tab === "raw" && <RawTab sessionId={sessionId} item={item} status={status} />}
            {tab === "diff" && diff !== undefined && <DiffView diff={diff} />}
            {tab === "console" && <Console progress={progress} live={status === "running"} />}
          </div>
        </div>
      )}
    </section>
  );
}

export default memo(ToolCard);

function InputTab({ input }: { input: unknown }) {
  const text = useMemo(() => JSON.stringify(input, null, 2) ?? "", [input]);
  if (input === undefined) {
    return <p className="text-xs text-muted">No input recorded for this call.</p>;
  }
  return (
    <div>
      <div className="mb-1 flex text-xs text-muted">
        <span className="grow" />
        <CopyButton text={text} />
      </div>
      <pre className="max-h-96 overflow-auto rounded border border-border bg-panel p-2 font-mono text-xs whitespace-pre-wrap">
        {text}
      </pre>
    </div>
  );
}

function ResultTab({
  item,
  status,
  meta,
}: {
  item: Card;
  status: CallStatus;
  meta: CallMeta | undefined;
}) {
  if (item.result === undefined) {
    return (
      <p className="text-xs text-muted">
        {status === "running"
          ? "Running…"
          : item.live
            ? `${meta?.summary ?? "done"} — the text the mentor reads is stored when the step ends.`
            : "No result stored for this call."}
      </p>
    );
  }
  return (
    <div>
      <div className="mb-1 flex items-center gap-2 text-xs text-muted">
        <span>
          {item.result.isError ? "error result" : "result"} · {bytes(item.result.text.length)} to
          the mentor
        </span>
        <span className="grow" />
        <CopyButton text={item.result.text} />
      </div>
      <ResultText text={item.result.text} />
    </div>
  );
}

function RawTab({
  sessionId,
  item,
  status,
}: {
  sessionId: string;
  item: Card;
  status: CallStatus;
}) {
  // A running call has no result event yet; look again once it ends.
  const key = `${item.callId}:${status}`;
  const [state, setState] = useState<
    | { key: string; kind: "error"; message: string }
    | { key: string; kind: "loaded"; raw: RawOutput }
  >();
  useEffect(() => {
    if (status === "running") return;
    let live = true;
    rawOutput(sessionId, item.callId, item.agentId).then(
      (raw) => {
        if (live) setState({ key, kind: "loaded", raw });
      },
      (e: unknown) => {
        if (live)
          setState({ key, kind: "error", message: e instanceof Error ? e.message : String(e) });
      },
    );
    return () => {
      live = false;
    };
  }, [sessionId, item.callId, item.agentId, status, key]);

  if (status === "running") return <p className="text-xs text-muted">Running…</p>;
  const current = state?.key === key ? state : undefined;
  if (current === undefined) return <p className="text-xs text-muted">Loading from the trace…</p>;
  if (current.kind === "error") return <p className="text-xs text-bad">{current.message}</p>;
  const { raw } = current;
  if (raw.text === undefined) {
    return (
      <p className="text-xs text-muted">
        This call produced no output blob (a failure or a denial).
      </p>
    );
  }
  const notes: string[] = [];
  if (raw.mediaType !== undefined) notes.push(raw.mediaType);
  if (raw.bytes !== undefined && raw.bytes !== raw.text.length)
    notes.push(`${bytes(raw.bytes)} produced`);
  if (raw.truncatedAtCapture === true) notes.push("cut at the capture limit");
  const note = notes.length > 0 ? notes.join(" · ") : undefined;
  return <RawView text={raw.text} {...(note === undefined ? {} : { note })} />;
}
