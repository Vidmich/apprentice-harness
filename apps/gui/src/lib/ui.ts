// Small browser-side helpers: the clipboard, external links, saving a
// file where the user picks.

import { save } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { isTauri } from "./bridge";
import { writeTextFile } from "./rpc";

/** Copies text; falls back to the legacy command where the async API is unavailable. */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    try {
      const area = document.createElement("textarea");
      area.value = text;
      area.style.position = "fixed";
      area.style.opacity = "0";
      document.body.appendChild(area);
      area.select();
      const ok = document.execCommand("copy");
      area.remove();
      return ok;
    } catch {
      return false;
    }
  }
}

/**
 * Saves `text` as a file the user names in a dialog (a download in a
 * plain browser). Resolves to the path, or `undefined` when cancelled.
 */
export async function saveTextFile(
  suggestedName: string,
  text: string,
): Promise<string | undefined> {
  if (!isTauri()) {
    const url = URL.createObjectURL(new Blob([text], { type: "application/json" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = suggestedName;
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
    return suggestedName;
  }
  const path = await save({ defaultPath: suggestedName, title: "Export session" });
  if (path === null) return undefined;
  await writeTextFile(path, text);
  return path;
}

/** Opens `http(s)`/`mailto` links in the OS browser; anything else is ignored. */
export async function openExternal(href: string): Promise<void> {
  if (!/^(https?:|mailto:)/i.test(href)) return;
  try {
    await openUrl(href);
  } catch (e) {
    console.warn("cannot open link:", e);
  }
}
