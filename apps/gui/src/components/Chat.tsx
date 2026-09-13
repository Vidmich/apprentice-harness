import { open } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useRef } from "react";
import { cancel, openSession, reloadSession, send } from "../lib/chat";
import { sessionTotals, thousands, usd } from "../lib/format";
import { addWorkspace } from "../lib/sessions";
import { promptOf } from "../lib/transcript";
import { type Chat as ChatTab, useStore } from "../store";
import { useTranscripts } from "../stores/transcripts";
import Composer, { type ComposerHandle } from "./chat/Composer";
import TranscriptView from "./chat/TranscriptView";

/** One chat: the header (workspace, title), the transcript, the composer. */
export default function Chat({ chat }: { chat: ChatTab }) {
  const updateChat = useStore((s) => s.updateChat);
  const connected = useStore((s) => s.daemon.connected);
  const transcript = useTranscripts((s) =>
    chat.sessionId === undefined ? undefined : s.transcripts[chat.sessionId],
  );
  const composer = useRef<ComposerHandle>(null);
  const sessionId = chat.sessionId;

  useEffect(() => {
    if (sessionId !== undefined && connected) void openSession(sessionId);
  }, [sessionId, connected]);

  // Ctrl+L focuses the composer; Esc cancels the run from anywhere in the chat.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "l" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        composer.current?.focus();
      } else if (e.key === "Escape" && sessionId !== undefined) {
        void cancel(sessionId);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [sessionId]);

  const phase = transcript?.live?.phase;
  const running = phase === "starting" || phase === "running" || phase === "cancelling";

  const onSend = useCallback(
    (prompt: string) => {
      updateChat(chat.id, { error: undefined });
      void send(chat.id, prompt);
    },
    [chat.id, updateChat],
  );
  const onCancel = useCallback(() => {
    if (sessionId !== undefined) void cancel(sessionId);
  }, [sessionId]);
  const onRetryAgent = useCallback(
    (agentId: string) => {
      if (transcript === undefined) return;
      const prompt = promptOf(transcript, agentId);
      if (prompt !== undefined) void send(chat.id, prompt);
    },
    [chat.id, transcript],
  );
  const onRetryPrompt = useCallback((prompt: string) => void send(chat.id, prompt), [chat.id]);

  const browse = async () => {
    const picked = await open({ directory: true, multiple: false, title: "Workspace folder" });
    if (typeof picked === "string") {
      updateChat(chat.id, { workspace: picked });
      // A folder picked here is a workspace like any other.
      void addWorkspace(picked).catch((e: unknown) => console.warn("workspace.add failed:", e));
    }
  };

  const workspace = transcript?.info?.workspace ?? chat.workspace;
  const info = transcript?.info;
  // The exact numbers behind the compact line, on hover.
  const totalsTitle =
    info === undefined
      ? ""
      : `input ${thousands(info.usage.input_tokens)} · output ${thousands(info.usage.output_tokens)} · cache read ${thousands(info.usage.cache_read_input_tokens)} · cache write ${thousands(info.usage.cache_creation_input_tokens)}${info.cost_usd === undefined ? " · cost unknown (an unpriced model)" : ` · ${usd(info.cost_usd)}`} · ${info.calls} mentor calls (title calls included)`;
  let disabledHint: string | undefined;
  if (!connected) disabledHint = "Waiting for the daemon…";
  else if (sessionId === undefined && chat.workspace.trim() === "") {
    disabledHint = "Choose a workspace folder to start";
  }

  return (
    <div className="flex h-full flex-col">
      <header className="flex items-center gap-2 border-b border-border px-4 py-2 text-sm">
        {sessionId === undefined ? (
          <>
            <label className="text-muted" htmlFor={`ws-${chat.id}`}>
              Workspace
            </label>
            <input
              id={`ws-${chat.id}`}
              className="input grow font-mono"
              placeholder="(choose a folder)"
              value={chat.workspace}
              onChange={(e) => updateChat(chat.id, { workspace: e.target.value })}
            />
            <button type="button" className="btn" onClick={() => void browse()}>
              Browse…
            </button>
          </>
        ) : (
          <>
            <span className="truncate font-medium">{transcript?.info?.title ?? "New chat"}</span>
            <span className="truncate font-mono text-xs text-muted" title={workspace ?? ""}>
              {workspace ?? "(no workspace)"}
            </span>
            <span className="grow" />
            {info !== undefined && info.calls > 0 && (
              <span
                className="shrink-0 font-mono text-[11px] text-muted"
                title={totalsTitle}
                data-testid="session-totals"
              >
                {sessionTotals(info.usage, info.cost_usd, info.calls)}
              </span>
            )}
            <span className="font-mono text-[11px] text-muted" title="Session id">
              {sessionId}
            </span>
            <button
              type="button"
              className="btn py-0 text-xs"
              onClick={() => void reloadSession(sessionId)}
              disabled={transcript?.loading === true}
              title="Re-read the stored transcript"
            >
              reload
            </button>
          </>
        )}
      </header>
      {chat.error !== undefined && (
        <p className="border-b border-border px-4 py-1 text-sm text-bad" role="alert">
          {chat.error}
        </p>
      )}
      {transcript !== undefined ? (
        <TranscriptView
          transcript={transcript}
          onRetryAgent={onRetryAgent}
          onRetryPrompt={onRetryPrompt}
        />
      ) : (
        <div className="min-h-0 grow px-4 py-2">
          <p className="mt-16 text-center text-sm text-muted">
            {sessionId === undefined
              ? "Pick a workspace folder, then ask the mentor something."
              : "loading…"}
          </p>
        </div>
      )}
      <Composer
        ref={composer}
        disabledHint={disabledHint}
        running={running}
        cancelling={phase === "cancelling" || phase === "starting"}
        onSend={onSend}
        onCancel={onCancel}
      />
    </div>
  );
}
