import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./panel.css";

type PanelSession = {
  pid: number;
  agent: string;
  cwd: string;
  terminal_program?: string | null;
};
type Tab = "information" | "activities" | "preview" | "notifications" | "schedule";
const TABS: Tab[] = ["information", "activities", "preview", "notifications", "schedule"];
const TAB_LABEL: Record<Tab, string> = {
  information: "Info",
  activities: "Activities",
  preview: "Preview",
  notifications: "Notifications",
  schedule: "Schedule",
};

type ScheduleItem =
  | {
      kind: "agent_cron";
      owner: string;
      expr: string;
      message: string;
      next_fires: string[];
      status: string;
    }
  | {
      kind: "agent_idle";
      owner: string;
      after_secs: number;
      cooldown_secs: number;
      message: string;
      status: string;
    }
  | {
      kind: "workflow";
      owner: string;
      expr?: string | null;
      next_fires: string[];
      status: string;
    }
  | {
      kind: "fleet";
      owner: string;
      trigger: string;
      next_fires: string[];
      status: string;
      budget_usd: number;
      autorun_env: boolean;
    };

type ScheduleStatus = { schedules: ScheduleItem[]; warnings: string[] };
type TokenTotals = { llm_calls: number; input_tokens: number; output_tokens: number };
type GitInfo = {
  root?: string | null;
  branch?: string | null;
  branches: string[];
  worktrees: string[];
  dirty: boolean;
};
type ProposalSummary = { file: string; modified: string };

type WorkParticipant = { id: string };
type ChannelSummary = {
  id: string;
  title: string;
  state: string;
  goal: string;
  created_at: string;
  updated_at: string;
  participants: WorkParticipant[];
  agents: string[];
  turns: number;
  preview: string;
};
type HitlRequestView = {
  channel_id: string;
  hitl_id: string;
  agent: string;
  summary: string;
  risk: string;
  ts: string;
};
type Activities = { channels: ChannelSummary[]; hitl: HitlRequestView[] };

const POLL_MS = 30000;

function scheduleWhen(s: ScheduleItem): string {
  switch (s.kind) {
    case "agent_cron":
      return s.expr;
    case "agent_idle":
      return `every ${s.after_secs}s idle`;
    case "workflow":
      return s.expr ?? "";
    case "fleet":
      return s.trigger;
  }
}

function scheduleNext(s: ScheduleItem): { text: string; title: string } {
  const fires = "next_fires" in s ? s.next_fires : [];
  if (!fires.length) return { text: "—", title: "" };
  const first = new Date(fires[0]);
  const text = Number.isNaN(first.getTime()) ? fires[0] : first.toLocaleString();
  return { text, title: fires.join("\n") };
}

export function PanelWindow() {
  const [sessions, setSessions] = useState<PanelSession[]>([]);
  const [pid, setPid] = useState<number | null>(null);
  const [tab, setTab] = useState<Tab>("information");
  const [previewTarget, setPreviewTarget] = useState<string | null>(null);
  const [showAll, setShowAll] = useState(false);

  const [gitInfo, setGitInfo] = useState<GitInfo | null>(null);
  const [cost, setCost] = useState<TokenTotals | null>(null);
  const [activities, setActivities] = useState<Activities | null>(null);
  const [schedule, setSchedule] = useState<ScheduleStatus | null>(null);
  const [proposals, setProposals] = useState<ProposalSummary[]>([]);

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

  // Fetch data for the active tab whenever the session, tab, showAll toggle
  // change, or the window regains focus.
  const fetchTabData = useRef<() => void>(() => {});
  fetchTabData.current = () => {
    if (!sess) return;
    if (tab === "information") {
      void invoke<GitInfo>("panel_git_info", { cwd: sess.cwd }).then(setGitInfo);
      void invoke<TokenTotals>("panel_cost", { agent: sess.agent }).then(setCost);
    } else if (tab === "activities") {
      void invoke<Activities>("panel_activities", { agent: sess.agent }).then(setActivities);
    } else if (tab === "notifications") {
      void invoke<ProposalSummary[]>("panel_proposals").then(setProposals);
    } else if (tab === "schedule") {
      void invoke<ScheduleStatus>("panel_schedule_status", {
        agent: showAll ? null : sess.agent,
      }).then(setSchedule);
    }
  };

  useEffect(() => {
    fetchTabData.current();
  }, [sess?.pid, sess?.agent, sess?.cwd, tab, showAll]);

  useEffect(() => {
    const onFocus = () => fetchTabData.current();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, []);

  // Poll activities/schedule every 30s while the window is visible.
  useEffect(() => {
    if (tab !== "activities" && tab !== "schedule") return;
    const id = setInterval(() => {
      if (document.visibilityState === "visible") fetchTabData.current();
    }, POLL_MS);
    return () => clearInterval(id);
  }, [tab]);

  const insert = (text: string) => {
    if (pid !== null) void invoke("panel_insert", { pid, text });
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
      {tab === "schedule" && sess && (
        <div className="panel-tab-header">
          <label className="panel-toggle">
            <input
              type="checkbox"
              checked={showAll}
              onChange={(e) => setShowAll(e.target.checked)}
            />
            Show all agents
          </label>
        </div>
      )}
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
              {gitInfo?.root && (
                <>
                  <dt>Git root</dt>
                  <dd>{gitInfo.root}</dd>
                  <dt>Branch</dt>
                  <dd>
                    {gitInfo.branch ?? "unknown"}
                    {gitInfo.dirty ? (
                      <span className="panel-badge panel-badge-warn"> dirty</span>
                    ) : (
                      <span className="panel-badge panel-badge-ok"> clean</span>
                    )}
                  </dd>
                  <dt>Branches</dt>
                  <dd>{gitInfo.branches.length}</dd>
                  <dt>Worktrees</dt>
                  <dd>{gitInfo.worktrees.length}</dd>
                </>
              )}
              {cost && (
                <>
                  <dt>LLM calls</dt>
                  <dd>{cost.llm_calls}</dd>
                  <dt>Input tokens</dt>
                  <dd>{cost.input_tokens}</dd>
                  <dt>Output tokens</dt>
                  <dd>{cost.output_tokens}</dd>
                </>
              )}
            </dl>
          </div>
        ) : tab === "activities" ? (
          <div className="panel-activities">
            <h3>Channels</h3>
            {!activities || activities.channels.length === 0 ? (
              <p className="panel-empty">No channels.</p>
            ) : (
              <ul className="panel-list">
                {activities.channels.map((c) => (
                  <li key={c.id} className="panel-list-row">
                    <span className="panel-list-name">{c.title || c.id}</span>
                    <span className="panel-badge">{c.state}</span>
                    <span className="panel-list-preview">{c.preview}</span>
                  </li>
                ))}
              </ul>
            )}
            <h3>Pending HITL</h3>
            {!activities || activities.hitl.length === 0 ? (
              <p className="panel-empty">No pending approvals.</p>
            ) : (
              <ul className="panel-list">
                {activities.hitl.map((h) => (
                  <li
                    key={`${h.channel_id}-${h.hitl_id}`}
                    className="panel-list-row panel-list-clickable"
                    onClick={() =>
                      insert(`mur channel approve ${h.channel_id} ${h.hitl_id}`)
                    }
                  >
                    <span className="panel-badge panel-badge-warn">{h.risk}</span>
                    <span className="panel-list-preview">{h.summary}</span>
                  </li>
                ))}
              </ul>
            )}
          </div>
        ) : tab === "notifications" ? (
          <div className="panel-notifications">
            <p className="panel-headline">
              {proposals.length} workflow proposal{proposals.length === 1 ? "" : "s"} pending
            </p>
            <ul className="panel-list">
              {proposals.map((p) => (
                <li
                  key={p.file}
                  className="panel-list-row panel-list-clickable"
                  onClick={() => insert("mur session out")}
                >
                  <span className="panel-list-name">{p.file}</span>
                  <span className="panel-list-preview">{p.modified}</span>
                </li>
              ))}
            </ul>
          </div>
        ) : tab === "schedule" ? (
          <div className="panel-schedule">
            <table className="panel-table">
              <thead>
                <tr>
                  <th>Kind</th>
                  <th>Owner</th>
                  <th>When</th>
                  <th>Next</th>
                  <th>Status</th>
                </tr>
              </thead>
              <tbody>
                {(schedule?.schedules ?? []).map((s, i) => {
                  const next = scheduleNext(s);
                  return (
                    <tr key={i}>
                      <td>{s.kind}</td>
                      <td>{s.owner}</td>
                      <td>{scheduleWhen(s)}</td>
                      <td title={next.title}>{next.text}</td>
                      <td>
                        <span
                          className={
                            s.status === "stopped"
                              ? "panel-badge panel-badge-warn"
                              : "panel-badge panel-badge-ok"
                          }
                        >
                          {s.status}
                        </span>
                        {s.kind === "fleet" && (
                          <span className="panel-muted">
                            {" "}
                            budget ${s.budget_usd.toFixed(2)}
                            {!s.autorun_env && " · autorun off"}
                          </span>
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
            {(schedule?.warnings.length ?? 0) > 0 && (
              <footer className="panel-warnings">
                {schedule?.warnings.map((w, i) => (
                  <p key={i} className="panel-muted">
                    {w}
                  </p>
                ))}
              </footer>
            )}
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
