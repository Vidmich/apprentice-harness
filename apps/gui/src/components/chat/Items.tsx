import { memo, useState } from "react";
import { clock, duration, usageLine } from "../../lib/format";
import { describe } from "../../lib/rpc";
import type { AgentTurn, Item } from "../../lib/transcript";
import { useStore } from "../../store";
import CopyButton from "./CopyButton";
import Markdown from "./Markdown";
import ToolCard from "./ToolCard";

type Of<K extends Item["kind"]> = Extract<Item, { kind: K }>;

export const UserItem = memo(function UserItem({ item }: { item: Of<"user"> }) {
  if (item.synthetic === true) {
    return (
      <p className="my-1 text-xs text-muted italic" role="note">
        {item.text}
      </p>
    );
  }
  return (
    <div
      className={`my-2 ml-auto max-w-[85%] rounded-lg bg-panel px-3 py-2 text-sm whitespace-pre-wrap ${item.pending === true ? "opacity-70" : ""}`}
      role="article"
      aria-label="Your message"
    >
      {item.text}
    </div>
  );
});

export const AssistantText = memo(function AssistantText({ item }: { item: Of<"assistant_text"> }) {
  return (
    <div className="group my-2 text-sm" role="article" aria-label="Mentor's answer">
      <Markdown text={item.text} streaming={item.streaming} />
      {item.streaming && <span className="animate-pulse text-muted">▍</span>}
      {!item.streaming && (
        <div className="h-4 opacity-0 transition-opacity group-hover:opacity-100">
          <CopyButton text={item.text} label="copy" />
        </div>
      )}
    </div>
  );
});

export const ThinkingItem = memo(function ThinkingItem({ item }: { item: Of<"thinking"> }) {
  const display = useStore((s) => s.thinkingDisplay);
  const [open, setOpen] = useState(false);
  if (display === "omitted") return null;
  const label =
    item.redacted === true
      ? "Thinking (redacted by the API)"
      : `Thinking${item.streaming ? "…" : ""} (${item.text.length.toLocaleString("en-US")} chars)`;
  return (
    <div className="my-1 text-xs text-muted">
      <button
        type="button"
        className="rounded px-1 hover:bg-panel hover:text-fg"
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        disabled={item.redacted === true}
      >
        {open ? "▾" : "▸"} {label}
      </button>
      {open && (
        <pre className="mt-1 max-h-72 overflow-auto rounded border border-border bg-panel p-2 font-sans whitespace-pre-wrap">
          {item.text}
        </pre>
      )}
    </div>
  );
});

const STATUS_LABEL: Record<AgentTurn["status"], string> = {
  running: "running",
  ok: "ok",
  cancelled: "cancelled",
  error: "error",
};

/** Model, duration, tokens and cost of one run, its warnings, and its error with Retry. */
export const TurnFooter = memo(function TurnFooter({
  item,
  now,
  onRetry,
}: {
  item: Of<"turn_footer">;
  now: number;
  onRetry: (agentId: string) => void;
}) {
  const { turn } = item;
  const configured = useStore((s) => s.model);
  const model = turn.model ?? configured;
  const elapsed = turn.startedAt === undefined ? undefined : (turn.endedAt ?? now) - turn.startedAt;
  const facts: string[] = [];
  if (model !== undefined) facts.push(model);
  if (turn.calls > 0 || turn.status === "running") {
    facts.push(usageLine(turn.usage, turn.costUsd, elapsed ?? 0));
  } else if (elapsed !== undefined) {
    facts.push(duration(elapsed));
  }
  if (turn.startedAt !== undefined) facts.push(clock(turn.startedAt));
  const statusClass =
    turn.status === "error" ? "text-bad" : turn.status === "cancelled" ? "text-warn" : "text-muted";
  // Retry re-sends the prompt; only where nothing ran that could have changed the workspace.
  const retry = turn.status === "error" && item.toolCalls === 0;
  return (
    <div
      className="my-1 border-b border-border pb-2 font-mono text-[11px] text-muted"
      role="contentinfo"
    >
      <div className="flex flex-wrap items-center gap-x-3">
        <span className={statusClass}>
          {turn.status === "running" ? (
            <span className="animate-pulse">● running</span>
          ) : (
            STATUS_LABEL[turn.status]
          )}
          {turn.truncated === true ? " (output cut at max_tokens)" : ""}
        </span>
        <span>{facts.join(" · ")}</span>
      </div>
      {turn.notes.map((n, i) => (
        <p key={i} className={n.kind === "warning" ? "text-warn" : ""}>
          {n.kind === "warning" ? "⚠ " : ""}
          {n.text}
        </p>
      ))}
      {turn.error !== undefined && (
        <p className="text-bad" role="alert">
          error: {describe(turn.error)}
          {retry && (
            <button
              type="button"
              className="btn ml-2 py-0 text-[11px]"
              onClick={() => onRetry(item.agentId)}
            >
              Retry
            </button>
          )}
        </p>
      )}
    </div>
  );
});

export const ErrorItem = memo(function ErrorItem({
  item,
  onRetry,
}: {
  item: Of<"error">;
  onRetry: (prompt: string) => void;
}) {
  return (
    <p className="my-2 rounded border border-bad/40 bg-bad/10 px-3 py-2 text-sm" role="alert">
      <span className="text-bad">error: {describe(item.error)}</span>
      {item.retry !== undefined && (
        <button
          type="button"
          className="btn ml-2 py-0 text-xs"
          onClick={() => onRetry(item.retry!)}
        >
          Retry
        </button>
      )}
    </p>
  );
});

export function ItemView({
  item,
  sessionId,
  now,
  onRetryAgent,
  onRetryPrompt,
}: {
  item: Item;
  sessionId: string;
  now: number;
  onRetryAgent: (agentId: string) => void;
  onRetryPrompt: (prompt: string) => void;
}) {
  switch (item.kind) {
    case "user":
      return <UserItem item={item} />;
    case "assistant_text":
      return <AssistantText item={item} />;
    case "thinking":
      return <ThinkingItem item={item} />;
    case "tool_call":
      return <ToolCard item={item} sessionId={sessionId} />;
    case "turn_footer":
      return (
        <TurnFooter
          item={item}
          now={item.turn.status === "running" ? now : 0}
          onRetry={onRetryAgent}
        />
      );
    case "error":
      return <ErrorItem item={item} onRetry={onRetryPrompt} />;
  }
}
