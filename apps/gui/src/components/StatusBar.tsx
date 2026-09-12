import { useState } from "react";
import { RpcFailure, daemonRestart } from "../lib/rpc";
import { authConfigured, useStore } from "../store";

export default function StatusBar() {
  const daemon = useStore((s) => s.daemon);
  const auth = useStore((s) => s.auth);
  const model = useStore((s) => s.model);
  const app = useStore((s) => s.app);
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
      <span className="grow" />
      <button className="btn py-0 text-xs" onClick={restart} disabled={restarting}>
        {restarting ? "restarting…" : "restart daemon"}
      </button>
      <span title={app?.log_file ?? ""}>gui {app?.version ?? ""}</span>
    </footer>
  );
}
