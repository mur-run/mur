import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./panel.css";

type PanelSession = {
  pid: number;
  agent: string;
  cwd: string;
  terminal_program?: string | null;
};
type Tab = "information" | "activities" | "preview" | "notifications";
const TABS: Tab[] = ["information", "activities", "preview", "notifications"];
const TAB_LABEL: Record<Tab, string> = {
  information: "Info",
  activities: "Activities",
  preview: "Preview",
  notifications: "Notifications",
};

export function PanelWindow() {
  const [sessions, setSessions] = useState<PanelSession[]>([]);
  const [pid, setPid] = useState<number | null>(null);
  const [tab, setTab] = useState<Tab>("information");
  const [previewTarget, setPreviewTarget] = useState<string | null>(null);

  useEffect(() => {
    void invoke<PanelSession[]>("panel_sessions").then((s) => {
      setSessions(s);
      setPid((cur) => cur ?? (s.length ? s[s.length - 1].pid : null));
    });
    const unSessions = listen<PanelSession[]>("panel-sessions", (e) => {
      setSessions(e.payload);
      setPid((cur) =>
        cur !== null && e.payload.some((s) => s.pid === cur)
          ? cur
          : e.payload.length
            ? e.payload[e.payload.length - 1].pid
            : null,
      );
    });
    const unFocus = listen<{ pid: number; tab: Tab }>("panel-focus", (e) => {
      setPid(e.payload.pid);
      setTab(e.payload.tab);
    });
    const unPreview = listen<{ pid: number; kind: string; target: string }>(
      "panel-preview",
      (e) => setPreviewTarget(e.payload.target),
    );
    return () => {
      unSessions.then((f) => f());
      unFocus.then((f) => f());
      unPreview.then((f) => f());
    };
  }, []);

  const sess = sessions.find((s) => s.pid === pid);
  const testInsert = () => {
    if (pid !== null) void invoke("panel_insert", { pid, text: "/help" });
  };

  return (
    <div className="panel-root">
      <header className="panel-header">
        <span className="panel-title">MUR Panel</span>
        <select
          value={pid ?? ""}
          onChange={(e) => setPid(e.target.value ? Number(e.target.value) : null)}
        >
          {sessions.map((s) => (
            <option key={s.pid} value={s.pid}>
              {s.agent} · {s.pid}
            </option>
          ))}
        </select>
      </header>
      <nav className="panel-tabs">
        {TABS.map((t) => (
          <button
            key={t}
            className={t === tab ? "panel-tab active" : "panel-tab"}
            onClick={() => setTab(t)}
          >
            {TAB_LABEL[t]}
          </button>
        ))}
      </nav>
      <main className="panel-body">
        {!sess ? (
          <p className="panel-empty">
            No live murmur session — run <code>murmur</code> and type{" "}
            <code>/panel</code>.
          </p>
        ) : tab === "information" ? (
          <div>
            <dl className="panel-info">
              <dt>Agent</dt>
              <dd>{sess.agent}</dd>
              <dt>Working dir</dt>
              <dd>{sess.cwd}</dd>
              <dt>Terminal</dt>
              <dd>{sess.terminal_program ?? "unknown"}</dd>
            </dl>
            {/* P1 demo affordance; replaced by real recommendations in P2. */}
            <button className="panel-test" onClick={testInsert}>
              Insert /help into murmur
            </button>
          </div>
        ) : tab === "preview" && previewTarget ? (
          <p className="panel-empty">
            Preview target: <code>{previewTarget}</code> (rendering lands in P3)
          </p>
        ) : (
          <p className="panel-empty">{TAB_LABEL[tab]} lands in a later phase.</p>
        )}
      </main>
    </div>
  );
}
