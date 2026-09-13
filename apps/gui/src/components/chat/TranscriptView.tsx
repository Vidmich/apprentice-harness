import { useVirtualizer } from "@tanstack/react-virtual";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { loadOlder } from "../../lib/chat";
import { type Item, type Transcript, deriveItems } from "../../lib/transcript";
import { MAX_PAGES, useJump } from "../../stores/jump";
import { ItemView } from "./Items";

/** Transcripts longer than this render through the virtualiser. */
export const VIRTUALIZE_FROM = 200;
/** Pixels from the top at which the older page loads. */
const LOAD_OLDER_PX = 240;
/** Pixels from the bottom within which the view follows new content. */
const STICK_PX = 48;

interface Props {
  transcript: Transcript;
  onRetryAgent: (agentId: string) => void;
  onRetryPrompt: (prompt: string) => void;
}

/**
 * The item list: sticks to the bottom while content streams (until the
 * user scrolls up), loads older pages near the top, and virtualises
 * long transcripts.
 */
export default function TranscriptView({ transcript, onRetryAgent, onRetryPrompt }: Props) {
  const items = useMemo(() => deriveItems(transcript), [transcript]);
  const scroller = useRef<HTMLDivElement>(null);
  const [stick, setStick] = useState(true);
  const [now, setNow] = useState(() => Date.now());
  const virtual = items.length > VIRTUALIZE_FROM;
  const live = transcript.live !== undefined && transcript.live.phase !== "done";

  // The running turn's clock.
  useEffect(() => {
    if (!live) return;
    const t = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(t);
  }, [live]);

  const virtualizer = useVirtualizer({
    count: virtual ? items.length : 0,
    getScrollElement: () => scroller.current,
    estimateSize: () => 96,
    overscan: 6,
    getItemKey: (i) => items[i]?.key ?? i,
  });

  // Keep the view at the bottom as content grows (a streaming delta, a
  // new item), unless the user scrolled away.
  const lastKey = items[items.length - 1]?.key;
  const lastText = lastTextOf(items);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el === null || !stick) return;
    if (virtual) virtualizer.scrollToIndex(items.length - 1, { align: "end" });
    else el.scrollTop = el.scrollHeight;
  }, [stick, virtual, items.length, lastKey, lastText, virtualizer]);

  // Loading an older page prepends rows: keep what was on screen where it was.
  const oldestSeq = transcript.messages[0]?.seq;
  const anchor = useRef<{ seq: number; height: number; top: number }>(undefined);
  useLayoutEffect(() => {
    const el = scroller.current;
    const a = anchor.current;
    if (el === null || a === undefined || oldestSeq === undefined || oldestSeq >= a.seq) return;
    anchor.current = undefined;
    if (!virtual) el.scrollTop = a.top + (el.scrollHeight - a.height);
  }, [oldestSeq, virtual]);

  // A jump to an agent's turn (from the usage panel): scroll to its
  // first item once it is here, loading older pages until it is.
  const jump = useJump((s) => s.target);
  const [flash, setFlash] = useState<string | undefined>(undefined);
  useEffect(() => {
    if (jump === undefined || jump.sessionId !== transcript.sessionId) return;
    const index = items.findIndex((it) => "agentId" in it && it.agentId === jump.agentId);
    if (index >= 0) {
      const item = items[index]!;
      useJump.getState().clear();
      setStick(false);
      if (virtual) virtualizer.scrollToIndex(index, { align: "start" });
      else {
        scroller.current
          ?.querySelector(`[data-key="${CSS.escape(item.key)}"]`)
          ?.scrollIntoView({ block: "start" });
      }
      setFlash(jump.agentId);
      const t = setTimeout(() => setFlash(undefined), 2000);
      return () => clearTimeout(t);
    }
    if (transcript.loading) return;
    if (!transcript.hasOlder || jump.pages >= MAX_PAGES) {
      useJump.getState().clear();
      return;
    }
    useJump.getState().paged();
    void loadOlder(transcript.sessionId);
  }, [
    jump,
    items,
    transcript.sessionId,
    transcript.loading,
    transcript.hasOlder,
    virtual,
    virtualizer,
  ]);

  const onScroll = useCallback(() => {
    const el = scroller.current;
    if (el === null) return;
    const fromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    setStick(fromBottom < STICK_PX);
    if (el.scrollTop < LOAD_OLDER_PX && transcript.hasOlder && !transcript.loading) {
      const first = transcript.messages[0];
      if (first !== undefined) {
        anchor.current = { seq: first.seq, height: el.scrollHeight, top: el.scrollTop };
        void loadOlder(transcript.sessionId);
      }
    }
  }, [transcript.hasOlder, transcript.loading, transcript.messages, transcript.sessionId]);

  const render = (item: Item) => (
    <div
      className={
        flash !== undefined && "agentId" in item && item.agentId === flash
          ? "jump-flash"
          : undefined
      }
    >
      <ItemView
        item={item}
        sessionId={transcript.sessionId}
        now={now}
        onRetryAgent={onRetryAgent}
        onRetryPrompt={onRetryPrompt}
      />
    </div>
  );

  return (
    <div
      ref={scroller}
      className="min-h-0 grow overflow-y-auto px-4 py-2"
      onScroll={onScroll}
      role="log"
      aria-label="Transcript"
    >
      {transcript.loading && <p className="py-1 text-center text-xs text-muted">loading…</p>}
      {transcript.loadError !== undefined && (
        <p className="py-1 text-center text-xs text-bad" role="alert">
          {transcript.loadError}
        </p>
      )}
      {!transcript.loading && transcript.hasOlder && (
        <p className="py-1 text-center text-xs text-muted">scroll up for earlier messages</p>
      )}
      {virtual ? (
        <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
          {virtualizer.getVirtualItems().map((v) => {
            const item = items[v.index];
            if (item === undefined) return null;
            return (
              <div
                key={v.key}
                data-index={v.index}
                ref={virtualizer.measureElement}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  transform: `translateY(${v.start}px)`,
                }}
              >
                {render(item)}
              </div>
            );
          })}
        </div>
      ) : (
        items.map((item) => (
          <div key={item.key} data-key={item.key}>
            {render(item)}
          </div>
        ))
      )}
      {items.length === 0 && !transcript.loading && transcript.loadError === undefined && (
        <p className="mt-16 text-center text-sm text-muted">Ask the mentor something.</p>
      )}
    </div>
  );
}

/** The text of a streaming last item, so a delta re-triggers the stick effect. */
function lastTextOf(items: Item[]): string | undefined {
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i]!;
    if (item.kind === "assistant_text" || item.kind === "thinking") return item.text;
    if (item.kind === "tool_call") return item.meta?.progressBytes.toString();
    if (item.kind !== "turn_footer") return undefined;
  }
  return undefined;
}
