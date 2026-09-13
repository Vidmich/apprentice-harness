import { type FormEvent, useCallback, useEffect, useState } from "react";
import type { ConfigLayer, ConfigPathResult, DaemonStatusResult } from "../../lib/api";
import { refreshDaemonFacts } from "../../lib/bootstrap";
import { bytes, uptime } from "../../lib/format";
import {
  RpcFailure,
  call,
  daemonRestart,
  dirSize,
  openPath,
  quitAndStopDaemon,
} from "../../lib/rpc";
import { SECTIONS, type Setting, loadSettings } from "../../lib/settings";
import { authConfigured, selectedWorkspace, useStore } from "../../store";
import RulesEditor from "./RulesEditor";
import SettingField from "./SettingField";

/**
 * The settings screen: sections on the left, the user layer or the
 * selected workspace's on top (only the overridable keys are editable
 * there), each key written as it changes.
 */
export default function Settings() {
  const [section, setSection] = useState(SECTIONS[0]!.id);
  const [layerChoice, setLayer] = useState<ConfigLayer>("user");
  const workspace = useStore(selectedWorkspace);
  const setView = useStore((s) => s.setView);
  const [values, setValues] = useState<Record<string, Setting>>({});
  const current = SECTIONS.find((s) => s.id === section) ?? SECTIONS[0]!;
  // The workspace tab needs a workspace.
  const layer: ConfigLayer = workspace === undefined ? "user" : layerChoice;
  const root = layer === "workspace" ? workspace?.root : undefined;

  const reload = useCallback(
    () =>
      loadSettings(
        SECTIONS.flatMap((s) => s.fields),
        root,
      ).then((v) => {
        setValues(v);
        void refreshDaemonFacts();
      }),
    [root],
  );

  useEffect(() => {
    void reload();
  }, [reload]);

  return (
    <div className="flex h-full min-h-0">
      <nav className="w-40 shrink-0 border-r border-border p-3" aria-label="Settings sections">
        <ul className="flex flex-col gap-0.5">
          {SECTIONS.map((s) => (
            <li key={s.id}>
              <button
                type="button"
                className={`w-full rounded px-2 py-1 text-left text-sm ${
                  s.id === section ? "bg-panel font-medium" : "hover:bg-panel/60"
                }`}
                aria-current={s.id === section ? "page" : undefined}
                onClick={() => setSection(s.id)}
              >
                {s.title}
              </button>
            </li>
          ))}
        </ul>
      </nav>
      <div className="flex min-w-0 grow flex-col">
        <header className="flex items-center gap-3 border-b border-border px-4 py-2 text-sm">
          <h1 className="font-semibold">Settings · {current.title}</h1>
          <span className="grow" />
          {workspace !== undefined && current.fields.length > 0 && (
            <div className="flex rounded border border-border text-xs" role="tablist">
              {(["user", "workspace"] as const).map((l) => (
                <button
                  key={l}
                  type="button"
                  role="tab"
                  aria-selected={layer === l}
                  className={`px-2 py-0.5 ${layer === l ? "bg-panel font-medium" : "text-muted"}`}
                  onClick={() => setLayer(l)}
                >
                  {l === "user" ? "User" : `Workspace: ${workspace.name}`}
                </button>
              ))}
            </div>
          )}
          <button type="button" className="btn py-0 text-xs" onClick={() => setView("chat")}>
            Back to chat
          </button>
        </header>
        <div className="min-h-0 grow overflow-y-auto px-4 py-2">
          {current.id === "mentor" && <MentorKey />}
          {current.fields.map((f) => (
            <SettingField
              key={`${f.key}:${layer}`}
              field={f}
              setting={values[f.key]}
              layer={layer}
              workspace={root}
              onSaved={() => void reload()}
            />
          ))}
          {current.id === "permissions" && (
            <section className="mt-4">
              <h2 className="mb-2 text-sm font-semibold">Rules</h2>
              <RulesEditor workspace={workspace?.root} />
            </section>
          )}
          {current.id === "data" && <DataSection />}
          {current.id === "about" && <AboutSection />}
        </div>
      </div>
    </div>
  );
}

/** The API key: status only, and a write-only field that replaces it. */
function MentorKey() {
  const auth = useStore((s) => s.auth);
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [saved, setSaved] = useState(false);
  const configured = authConfigured(auth);
  const provider = auth?.providers.find((p) => p.name === "anthropic");

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const trimmed = key.trim();
    if (trimmed === "") return;
    setBusy(true);
    setError(undefined);
    setSaved(false);
    try {
      await call("auth.set_key", { provider: "anthropic", key: trimmed });
      setKey("");
      setSaved(true);
      await refreshDaemonFacts();
    } catch (err) {
      setError(err instanceof RpcFailure ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} className="flex flex-col gap-1 border-b border-border py-2">
      <div className="flex items-center gap-2">
        <label htmlFor="set-key" className="w-48 shrink-0 text-sm">
          Anthropic API key
        </label>
        <input
          id="set-key"
          className="input grow font-mono"
          type="password"
          autoComplete="off"
          spellCheck={false}
          value={key}
          placeholder={configured ? "(configured — paste a new key to replace it)" : "sk-ant-…"}
          onChange={(e) => setKey(e.target.value)}
        />
        <button className="btn" type="submit" disabled={busy || key.trim() === ""}>
          {busy ? "Saving…" : "Save key"}
        </button>
      </div>
      <p className="pl-50 text-xs text-muted">
        {configured === undefined
          ? "…"
          : configured
            ? `Configured (${provider?.source ?? "?"}). The key is never shown; the daemon keeps it in the OS keychain.`
            : "Not configured. The daemon stores it in the OS keychain; this window never writes it to disk."}
        {saved ? " Saved." : ""}
      </p>
      {error !== undefined && (
        <p className="pl-50 text-xs text-bad" role="alert">
          {error}
        </p>
      )}
    </form>
  );
}

/** Paths from `config.path`, the data dir's size, the export link. */
function DataSection() {
  const [paths, setPaths] = useState<ConfigPathResult>();
  const [size, setSize] = useState<number>();
  const [error, setError] = useState<string>();
  useEffect(() => {
    call("config.path", {})
      .then((p) => {
        setPaths(p);
        return dirSize(p.data_dir).then(setSize);
      })
      .catch((e: unknown) => setError(e instanceof RpcFailure ? e.message : String(e)));
  }, []);
  const open = (p: string) => void openPath(p).catch((e: unknown) => setError(String(e)));
  return (
    <div className="flex flex-col gap-2 py-2 text-sm">
      <Row label="Config file">
        <span className="truncate font-mono text-xs">{paths?.config_file ?? "…"}</span>
        {paths !== undefined && (
          <button
            type="button"
            className="btn py-0 text-xs"
            onClick={() => open(paths.config_file)}
          >
            open
          </button>
        )}
      </Row>
      <Row label="Data directory">
        <span className="truncate font-mono text-xs">{paths?.data_dir ?? "…"}</span>
        {size !== undefined && <span className="text-xs text-muted">{bytes(size)}</span>}
        {paths !== undefined && (
          <button type="button" className="btn py-0 text-xs" onClick={() => open(paths.data_dir)}>
            open folder
          </button>
        )}
      </Row>
      <Row label="Traces">
        <span className="text-xs text-muted">Export with redaction comes with M01-14.</span>
      </Row>
      {error !== undefined && (
        <p className="text-xs text-bad" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

/** Versions from `daemon.status` and the app, the log files, the daemon buttons. */
function AboutSection() {
  const app = useStore((s) => s.app);
  const daemon = useStore((s) => s.daemon);
  const [status, setStatus] = useState<DaemonStatusResult>();
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!daemon.connected) return;
    call("daemon.status", {})
      .then(setStatus)
      .catch((e: unknown) => setError(e instanceof RpcFailure ? e.message : String(e)));
  }, [daemon.connected]);
  const open = (p: string) => void openPath(p).catch((e: unknown) => setError(String(e)));
  const act = (f: () => Promise<void>) => {
    setBusy(true);
    f()
      .catch((e: unknown) => setError(e instanceof RpcFailure ? e.message : String(e)))
      .finally(() => setBusy(false));
  };
  return (
    <div className="flex flex-col gap-2 py-2 text-sm">
      <Row label="App">
        <span>apprentice-harness {app?.version ?? "…"}</span>
        {app?.log_file !== undefined && (
          <button type="button" className="btn py-0 text-xs" onClick={() => open(app.log_file!)}>
            log file
          </button>
        )}
      </Row>
      <Row label="Daemon">
        {daemon.connected ? (
          <span>
            harnessd {status?.version ?? daemon.version} · pid {status?.pid ?? daemon.pid}
            {status !== undefined &&
              ` · up ${uptime(status.uptime_s)} · ${status.sessions_open} sessions open`}
          </span>
        ) : (
          <span className="text-muted">not connected</span>
        )}
        {status?.log_file !== undefined && (
          <button type="button" className="btn py-0 text-xs" onClick={() => open(status.log_file!)}>
            log file
          </button>
        )}
      </Row>
      <Row label="">
        <button
          type="button"
          className="btn py-0 text-xs"
          disabled={busy}
          onClick={() => act(daemonRestart)}
        >
          restart daemon
        </button>
        <button
          type="button"
          className="btn py-0 text-xs"
          disabled={busy}
          title="Closing the window alone leaves the daemon running"
          onClick={() => act(quitAndStopDaemon)}
        >
          quit and stop the daemon
        </button>
      </Row>
      {error !== undefined && (
        <p className="text-xs text-bad" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center gap-2">
      <span className="w-48 shrink-0">{label}</span>
      <div className="flex min-w-0 grow items-center gap-2">{children}</div>
    </div>
  );
}
