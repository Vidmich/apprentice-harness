// Placeholder for the typed JSON-RPC bridge to the daemon (implemented in M00-10).
// All daemon communication goes through this module; components never call Tauri directly.

export const appVersion: string = "0.1.0";

export interface RpcError {
  code: number;
  message: string;
  data?: { kind: string; details?: unknown };
}
