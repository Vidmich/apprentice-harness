import { type FormEvent, useState } from "react";
import { refreshDaemonFacts } from "../lib/bootstrap";
import { RpcFailure, call } from "../lib/rpc";

/**
 * First-run screen: takes the mentor API key and hands it straight to the
 * daemon (`auth.set_key`). The key is never kept on this side.
 */
export default function Setup() {
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const trimmed = key.trim();
    if (trimmed === "") return;
    setBusy(true);
    setError(undefined);
    try {
      await call("auth.set_key", { provider: "anthropic", key: trimmed });
      setKey("");
      await refreshDaemonFacts();
    } catch (err) {
      setError(err instanceof RpcFailure ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mx-auto mt-16 max-w-md">
      <h1 className="mb-2 text-lg font-semibold">Set up the mentor</h1>
      <p className="mb-4 text-sm text-muted">
        apprentice-harness calls Claude through your Anthropic API key. The daemon stores it in the
        OS keychain; this window never writes it to disk.
      </p>
      <form onSubmit={submit} className="flex flex-col gap-3">
        <label className="flex flex-col gap-1 text-sm">
          Anthropic API key
          <input
            className="input font-mono"
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={key}
            onChange={(e) => setKey(e.target.value)}
            placeholder="sk-ant-…"
            autoFocus
          />
        </label>
        {error !== undefined && <p className="text-sm text-bad">{error}</p>}
        <div>
          <button className="btn-primary" type="submit" disabled={busy || key.trim() === ""}>
            {busy ? "Saving…" : "Save key"}
          </button>
        </div>
      </form>
    </div>
  );
}
