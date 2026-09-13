// Small browser-side helpers: the clipboard and external links.

import { openUrl } from "@tauri-apps/plugin-opener";

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

/** Opens `http(s)`/`mailto` links in the OS browser; anything else is ignored. */
export async function openExternal(href: string): Promise<void> {
  if (!/^(https?:|mailto:)/i.test(href)) return;
  try {
    await openUrl(href);
  } catch (e) {
    console.warn("cannot open link:", e);
  }
}
