// The typed JSON-RPC bridge to the daemon. All daemon communication goes
// through this module; components never call Tauri directly. The backend
// commands are untyped passthroughs (`rpc_call`, `rpc_stream`); the types
// live in `api.ts`.

import {
  type MethodName,
  type Methods,
  type RpcError,
  type StreamingMethod,
  isRpcError,
} from "./api";
import { invoke } from "./bridge";

export { API_VERSION } from "./api";

/** A daemon (or bridge) error, thrown by every function here. */
export class RpcFailure extends Error {
  readonly error: RpcError;

  constructor(error: RpcError) {
    super(describe(error));
    this.name = "RpcFailure";
    this.error = error;
  }

  get kind(): string | undefined {
    return this.error.data?.kind;
  }
}

/** `message [kind]`, the same rendering the CLI uses. */
export function describe(e: RpcError): string {
  return e.data?.kind ? `${e.message} [${e.data.kind}]` : e.message;
}

/** Wraps whatever `invoke` rejected with into an `RpcFailure`. */
function toFailure(e: unknown): RpcFailure {
  if (e instanceof RpcFailure) return e;
  if (isRpcError(e)) return new RpcFailure(e);
  const message = e instanceof Error ? e.message : String(e);
  return new RpcFailure({ code: -32603, message, data: { kind: "bridge" } });
}

async function bridge<T>(command: string, args: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (e) {
    throw toFailure(e);
  }
}

/** Calls a daemon method. */
export function call<M extends MethodName>(
  method: M,
  params: Methods[M]["params"],
): Promise<Methods[M]["result"]> {
  return bridge<Methods[M]["result"]>("rpc_call", { method, params });
}

export interface StreamStarted<M extends StreamingMethod> {
  /** The daemon subscription whose events arrive on the channel. */
  subscription: string;
  result: Methods[M]["result"];
}

/**
 * Calls a streaming method; its events arrive on `rpc:event:<channel>`.
 * Pick the channel with `events.newChannel()` and `events.subscribe` to it
 * *before* calling, so no event can slip past.
 */
export function stream<M extends StreamingMethod>(
  method: M,
  params: Methods[M]["params"],
  channel: string,
): Promise<StreamStarted<M>> {
  return bridge<StreamStarted<M>>("rpc_stream", { method, params, channel });
}

/** Connection state as published by the backend (`daemon:status`). */
export interface DaemonStatus {
  connected: boolean;
  version?: string;
  pid?: number;
  error?: string;
  spawned: boolean;
}

export function daemonStatus(): Promise<DaemonStatus> {
  return bridge<DaemonStatus>("daemon_status", {});
}

/** Stops the daemon; the backend reconnects (spawning a fresh one). */
export function daemonRestart(): Promise<void> {
  return bridge<void>("daemon_restart", {});
}

export interface AppInfo {
  version: string;
  config_file: string;
  data_dir: string;
  log_file?: string;
}

export function appInfo(): Promise<AppInfo> {
  return bridge<AppInfo>("app_info", {});
}

/** Opens a file or folder with the OS default (the editor for a rules file). */
export function openPath(path: string): Promise<void> {
  return bridge<void>("open_path", { path });
}

/** Writes a text file where the user chose to save (an export). */
export function writeTextFile(path: string, contents: string): Promise<void> {
  return bridge<void>("write_text_file", { path, contents });
}

/** Bytes under a directory (the data dir's size in Settings). */
export function dirSize(path: string): Promise<number> {
  return bridge<number>("dir_size", { path });
}

/** Stops the daemon and quits the app (the tray's "Quit and stop the daemon"). */
export function quitAndStopDaemon(): Promise<void> {
  return bridge<void>("quit_and_stop_daemon", {});
}
