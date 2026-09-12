import { open } from "@tauri-apps/plugin-dialog";
import { useEffect, useRef, useState } from "react";
import { cancelRun, startRun } from "../lib/playground";
import { describe } from "../lib/rpc";
import { runUsageLine } from "../lib/run";
import { type Tab, useStore } from "../store";

/** A prompt box that runs `agent.run` in a fresh session and streams the reply. */
export default function Playground({ tab }: { tab: Tab }) {
  const updateTab = useStore((s) => s.updateTab);
  const connected = useStore((s) => s.daemon.connected);
  const [showThinking, setShowThinking] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const output = useRef<HTMLPreElement>(null);
  const { run } = tab;
  const busy = run.phase === "starting" || run.phase === "running" || run.phase === "cancelling";

  // Tick the elapsed time while the run is live.
  useEffect(() => {
    if (!busy) return;
    const t = setInterval(() => setNow(Date.now()), 250);
    return () => clearInterval(t);
  }, [busy]);

  // Follow the streamed text.
  useEffect(() => {
    const el = output.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [run.text]);

  const browse = async () => {
    const picked = await open({ directory: true, multiple: false, title: "Workspace folder" });
    if (typeof picked === "string") updateTab(tab.id, { workspace: picked });
  };

  const onKey = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey) && !busy) {
      e.preventDefault();
      void startRun(tab.id);
    }
  };

  return (
    <div className="flex h-full flex-col gap-3 p-4">
      <div className="flex items-center gap-2">
        <label className="text-sm text-muted" htmlFor={`ws-${tab.id}`}>
          Folder
        </label>
        <input
          id={`ws-${tab.id}`}
          className="input grow font-mono"
          placeholder="(none)"
          value={tab.workspace}
          onChange={(e) => updateTab(tab.id, { workspace: e.target.value })}
          disabled={busy}
        />
        <button className="btn" onClick={browse} disabled={busy}>
          Browse…
        </button>
      </div>
      <textarea
        className="input min-h-24 resize-y font-sans"
        placeholder="Ask the mentor something… (Ctrl+Enter to run)"
        value={tab.prompt}
        onChange={(e) => updateTab(tab.id, { prompt: e.target.value })}
        onKeyDown={onKey}
        disabled={busy}
      />
      <div className="flex items-center gap-2">
        {busy ? (
          <button
            className="btn"
            onClick={() => void cancelRun(tab.id)}
            disabled={run.phase !== "running"}
          >
            {run.phase === "cancelling" ? "Cancelling…" : "Cancel"}
          </button>
        ) : (
          <button
            className="btn-primary"
            onClick={() => void startRun(tab.id)}
            disabled={!connected || tab.prompt.trim() === ""}
          >
            Run
          </button>
        )}
        <span className="text-sm text-muted">{phaseLabel(tab)}</span>
        <span className="grow" />
        <label className="flex items-center gap-1 text-xs text-muted">
          <input
            type="checkbox"
            checked={showThinking}
            onChange={(e) => setShowThinking(e.target.checked)}
          />
          show thinking
        </label>
      </div>
      {showThinking && run.thinking !== "" && (
        <pre className="max-h-40 overflow-auto rounded border border-border bg-panel p-2 font-mono text-xs whitespace-pre-wrap text-muted">
          {run.thinking}
        </pre>
      )}
      <pre
        ref={output}
        className="min-h-0 grow overflow-auto rounded border border-border bg-panel p-3 font-mono text-sm whitespace-pre-wrap"
      >
        {run.text}
      </pre>
      {run.activity.length > 0 && (
        <ul className="max-h-32 overflow-auto font-mono text-xs text-muted">
          {run.activity.map((line, i) => (
            <li key={i}>{line}</li>
          ))}
        </ul>
      )}
      {run.error !== undefined && <p className="text-sm text-bad">error: {describe(run.error)}</p>}
      {run.phase !== "idle" && (
        <p className="font-mono text-xs text-muted">{runUsageLine(run, now)}</p>
      )}
    </div>
  );
}

function phaseLabel(tab: Tab): string {
  const { run } = tab;
  switch (run.phase) {
    case "idle":
      return "";
    case "starting":
      return "starting…";
    case "running":
      return `running (session ${run.sessionId ?? "?"})`;
    case "cancelling":
      return "cancelling…";
    case "done":
      return `status: ${run.status ?? "?"}${run.truncated ? " (output truncated at max_tokens)" : ""}`;
  }
}
