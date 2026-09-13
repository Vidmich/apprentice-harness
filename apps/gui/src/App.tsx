import { useEffect } from "react";
import Chat from "./components/Chat";
import PermissionDialog from "./components/PermissionDialog";
import Settings from "./components/settings/Settings";
import Setup from "./components/Setup";
import Sidebar from "./components/Sidebar";
import StatusBar from "./components/StatusBar";
import Toasts from "./components/Toasts";
import { pendingFor } from "./lib/permissions";
import { newSession } from "./lib/sessions";
import { authConfigured, useStore } from "./store";
import { usePermissions } from "./stores/permissions";
import { useTranscripts } from "./stores/transcripts";

export default function App() {
  const daemon = useStore((s) => s.daemon);
  const auth = useStore((s) => s.auth);
  const chats = useStore((s) => s.chats);
  const activeChat = useStore((s) => s.activeChat);
  const view = useStore((s) => s.view);
  const setView = useStore((s) => s.setView);
  const permissions = usePermissions((s) => s.state);
  const configured = authConfigured(auth);
  const chat = chats.find((c) => c.id === activeChat) ?? chats[0];
  const activeSession = view === "chat" ? chat?.sessionId : undefined;
  const transcript = useTranscripts((s) =>
    activeSession === undefined ? undefined : s.transcripts[activeSession],
  );

  // Ctrl+N: a new session on the selected workspace; Ctrl+,: settings.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
      if (e.key === "n") {
        e.preventDefault();
        newSession();
      } else if (e.key === ",") {
        e.preventDefault();
        setView(view === "settings" ? "chat" : "settings");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [view, setView]);

  const asking = activeSession === undefined ? [] : pendingFor(permissions, activeSession);
  const hasWorkspace =
    (transcript?.info?.workspace ?? chat?.workspace ?? "") !== "" &&
    transcript?.info?.workspace !== null;

  let main: React.ReactNode;
  if (!daemon.connected) {
    main = (
      <div className="mx-auto mt-16 max-w-md text-center text-sm text-muted">
        <p className="mb-2 text-base text-fg">Connecting to the daemon…</p>
        {daemon.error !== undefined && <p>{daemon.error}</p>}
      </div>
    );
  } else if (configured === false && view !== "settings") {
    main = <Setup />;
  } else if (view === "settings") {
    main = <Settings />;
  } else if (chat !== undefined) {
    main = <Chat key={chat.id} chat={chat} />;
  }

  return (
    <div className="grid h-full grid-cols-[260px_1fr] grid-rows-[1fr_auto]">
      <Sidebar />
      <main className="relative min-h-0 overflow-hidden">
        {main}
        {asking[0] !== undefined && (
          <PermissionDialog
            key={asking[0].requestId}
            request={asking[0]}
            hasWorkspace={hasWorkspace}
          />
        )}
        <Toasts activeSession={activeSession} />
      </main>
      <div className="col-span-2">
        <StatusBar />
      </div>
    </div>
  );
}
