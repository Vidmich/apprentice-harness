import { useState } from "react";
import { RpcFailure, daemonRestart } from "../lib/rpc";
import { isBusy } from "../lib/transcript";
import { authConfigured, useStore } from "../store";
import { useSessions } from "../stores/sessions";
import { useTranscripts } from "../stores/transcripts";

/** Sessions with a run in flight: the daemon's word plus what streams here. */
function useRunningCount(): number {
  const sessions = useSessions((s) => s.sessions);
  const transcripts = useTranscripts((s) => s.transcripts);
  const running = new Set<string>();
  for (const s of sessions) if (s.running_agent !== undefined) running.add(s.id);
  for (const [id, t] of Object.entries(transcripts)) if (isBusy(t)) running.add(id);
  return running.size;
}

export default function StatusBar() {
  const daemon = useStore((s) => s.daemon);
  const auth = useStore((s) => s.auth);
  const model = useStore((s) => s.model);
  const app = useStore((s) => s.app);
  const running = useRunningCount();
  const [restarting, setRestarting] = useState(false);
  const configured = authConfigured(auth);
  const provider = auth?.providers.find((p) => p.name === "anthropic");

  const restart = async () => {
    setRestarting(true);
    try {
      await daemonRestart();
    } catch (e) {
      console.warn("restart failed:", e instanceof RpcFailure ? e.message : e);
    } finally {
      setRestarting(false);
    }
  };

  return (
    <footer className="flex h-7 items-center gap-4 border-t border-border px-3 text-xs text-muted">
      <span className="flex items-center gap-1.5" title={daemon.error ?? ""}>
        <span
          className={`inline-block h-2 w-2 rounded-full ${daemon.connected ? "bg-ok" : "bg-bad"}`}
          aria-hidden
        />
        {daemon.connected ? (
          <>
            daemon v{daemon.version} · pid {daemon.pid}
            {daemon.spawned ? " (started by the app)" : ""}
          </>
        ) : (
          <>disconnected{daemon.error ? `: ${daemon.error}` : ""}</>
        )}
      </span>
      <span>
        key:{" "}
        {configured === undefined
          ? "…"
          : configured
            ? `configured (${provider?.source ?? "?"})`
            : "not configured"}
      </span>
      <span>model: {model ?? "…"}</span>
      {running > 0 && (
        <span className="flex items-center gap-1 text-accent" title="sessions with a run in flight">
          <span className="inline-block h-2 w-2 animate-pulse rounded-full bg-accent" aria-hidden />
          {running} running
        </span>
      )}
      <span className="grow" />
      <button className="btn py-0 text-xs" onClick={restart} disabled={restarting}>
        {restarting ? "restarting…" : "restart daemon"}
      </button>
      <span title={app?.log_file ?? ""}>gui {app?.version ?? ""}</span>
    </footer>
  );
}
