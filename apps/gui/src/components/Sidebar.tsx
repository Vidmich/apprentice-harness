import { newSession } from "../lib/sessions";
import { useStore } from "../store";
import SessionList from "./sidebar/SessionList";
import WorkspaceSwitcher from "./sidebar/WorkspaceSwitcher";

/** Left pane: the workspace switcher, the new-session and settings buttons, the sessions. */
export default function Sidebar() {
  const view = useStore((s) => s.view);
  const setView = useStore((s) => s.setView);
  const connected = useStore((s) => s.daemon.connected);

  return (
    <aside className="flex min-h-0 flex-col gap-3 border-r border-border bg-panel p-3">
      <WorkspaceSwitcher />
      <div className="flex items-center gap-1">
        <button
          type="button"
          className="btn grow"
          onClick={newSession}
          disabled={!connected}
          title="New session (Ctrl+N)"
        >
          + New session
        </button>
        <button
          type="button"
          className={`btn px-2 ${view === "settings" ? "border-accent" : ""}`}
          onClick={() => setView(view === "settings" ? "chat" : "settings")}
          title="Settings (Ctrl+,)"
          aria-label="Settings"
          aria-pressed={view === "settings"}
        >
          ⚙
        </button>
      </div>
      <SessionList />
    </aside>
  );
}
