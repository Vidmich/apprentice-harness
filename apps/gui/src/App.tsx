import { appVersion } from "./lib/rpc";

export default function App() {
  return (
    <div className="shell">
      <aside className="sidebar">
        <h2>Sessions</h2>
      </aside>
      <main className="main">
        <p className="muted">apprentice-harness — GUI shell (M00-01)</p>
      </main>
      <footer className="statusbar">gui {appVersion}</footer>
    </div>
  );
}
