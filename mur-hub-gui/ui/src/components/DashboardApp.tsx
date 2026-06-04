import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save } from "@tauri-apps/plugin-dialog";
import { useAgents } from "../context/AgentContext";
import type { AgentEntry, AgentRuntimeStatus, RuntimeState } from "../types";
import { WizardModal } from "./wizard/WizardModal";
import { PresetImportModal } from "./PresetImportModal";
import { MuragentImportModal } from "./MuragentImportModal";
import { useUnreadCount } from "./CompanionInbox";
import { DetailPanel } from "./DetailPanel";
import { Mascot } from "./Mascot";
import { useT } from "../i18n";

// ─── Shared helpers ────────────────────────────────────────────────────────

const CATEGORY_COLORS: Record<string, string> = {
  research: "#4F46E5",
  automation: "#059669",
  monitor: "#D97706",
  notify: "#DC2626",
  commerce: "#7C3AED",
  custom: "#6B7280",
};

const ALL_CATEGORIES = ["research", "automation", "monitor", "notify", "commerce", "custom"];

function avatarInitials(displayName: string): string {
  return displayName
    .split(" ")
    .slice(0, 2)
    .map((w) => w[0]?.toUpperCase() ?? "")
    .join("");
}

function showToast(msg: string) {
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = msg;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), 2000);
}

// Maps a runtime state to a status pill { className, statusKey } for the new brand.
function runtimePill(rt: RuntimeState | undefined): {
  cls: string;
  key: "status.running" | "status.idle" | "status.error";
} {
  switch (rt?.state) {
    case "running":
    case "restarting":
      return { cls: "pill pill--run", key: "status.running" };
    case "failed":
      return { cls: "pill pill--fail", key: "status.error" };
    default:
      return { cls: "pill pill--idle", key: "status.idle" };
  }
}

// ─── GridCard ──────────────────────────────────────────────────────────────

interface GridCardProps {
  agent: AgentEntry;
  runtime: AgentRuntimeStatus | undefined;
  isSelected: boolean;
}

export function GridCard({ agent, runtime, isSelected }: GridCardProps) {
  const { t } = useT();
  const { setSelected } = useAgents();
  const unread = useUnreadCount(agent.name);
  const color = CATEGORY_COLORS[agent.category] ?? "#6B7280";
  const pill = runtimePill(runtime?.state);
  const isRunning = runtime?.state.state === "running";
  const isBusy = runtime?.state.state === "restarting";

  // Drag-to-spawn state
  const holdTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [dragging, setDragging] = useState(false);
  const [ghostPos, setGhostPos] = useState({ x: 0, y: 0 });
  const cursorOutsideRef = useRef(false);
  const mouseDownPosRef = useRef({ x: 0, y: 0 });

  async function handleRun() {
    await invoke("start_agent", { name: agent.name }).catch((e) =>
      showToast(`Failed: ${e}`),
    );
  }
  async function handleStop() {
    await invoke("stop_agent", { name: agent.name }).catch((e) =>
      showToast(`Failed: ${e}`),
    );
  }
  async function handleShare() {
    const outPath = await save({
      defaultPath: `${agent.name}.muragent`,
      filters: [{ name: "MuR Agent", extensions: ["muragent"] }],
    });
    if (!outPath) return;
    invoke<string>("export_muragent_file", { name: agent.name, outPath })
      .then(() => showToast(`Exported ${agent.name}.muragent`))
      .catch((e) => showToast(`Export failed: ${e}`));
  }

  function startHold(e: React.MouseEvent) {
    if (e.button !== 0) return;
    mouseDownPosRef.current = { x: e.clientX, y: e.clientY };
    holdTimer.current = setTimeout(() => {
      setDragging(true);
      setGhostPos({ x: e.screenX, y: e.screenY });
      cursorOutsideRef.current = false;
    }, 300);
  }

  function cancelHold() {
    if (holdTimer.current) {
      clearTimeout(holdTimer.current);
      holdTimer.current = null;
    }
  }

  useEffect(() => {
    if (!dragging) return;

    function onMove(e: MouseEvent) {
      setGhostPos({ x: e.screenX, y: e.screenY });
    }
    function onLeave() { cursorOutsideRef.current = true; }
    function onEnter() { cursorOutsideRef.current = false; }
    function onUp(e: MouseEvent) {
      setDragging(false);
      if (cursorOutsideRef.current) {
        invoke("pet_spawn_at", {
          agentName: agent.name,
          screenX: e.screenX,
          screenY: e.screenY,
        }).catch((err) => showToast(`Pet: ${err}`));
      }
    }
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    document.addEventListener("mouseleave", onLeave);
    document.addEventListener("mouseenter", onEnter);
    return () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      document.removeEventListener("mouseleave", onLeave);
      document.removeEventListener("mouseenter", onEnter);
    };
  }, [dragging, agent.name]);

  return (
    <>
      <div
        className={`grid-card${isSelected ? " grid-card--selected" : ""}${dragging ? " grid-card--dragging" : ""}`}
        style={{ ["--cat" as string]: color }}
        data-agent={agent.name}
        onMouseDown={startHold}
        onMouseUp={cancelHold}
        onMouseLeave={cancelHold}
        onClick={() => setSelected(isSelected ? null : agent.name)}
      >
        <div className="grid-card__head">
          <div className="grid-card__avatar" style={{ background: color }}>
            {avatarInitials(agent.display_name)}
            {unread > 0 && (
              <span className="unread-badge">{unread > 99 ? "99+" : unread}</span>
            )}
          </div>
          <div>
            <p className="grid-card__name">{agent.display_name}</p>
            <p className="grid-card__cat">{agent.category}</p>
          </div>
        </div>
        <div className="grid-card__actions">
          <button
            disabled={isRunning || isBusy}
            onClick={handleRun}
            title={t("dashboard.runTooltip")}
          >
            ▶ {t("dashboard.run")}
          </button>
          <button
            disabled={!isRunning && !isBusy}
            onClick={handleStop}
            title={t("dashboard.stopTooltip")}
          >
            ■ {t("dashboard.stop")}
          </button>
          <button onClick={handleShare} title={t("dashboard.shareTooltip")}>
            ↑ {t("dashboard.share")}
          </button>
        </div>
        <div className="grid-card__foot">
          <span className={pill.cls}>
            <span className="pill__dot" />
            {t(pill.key)}
          </span>
          <span className="grid-card__open">{t("dashboard.open")} →</span>
        </div>
      </div>

      {dragging && (
        <div
          className="drag-ghost"
          style={{
            position: "fixed",
            left: ghostPos.x - window.screenX - 40,
            top: ghostPos.y - window.screenY - 40,
            pointerEvents: "none",
          }}
        >
          <div className="drag-ghost-avatar" style={{ background: color }}>
            {avatarInitials(agent.display_name)}
          </div>
          <span className="drag-ghost-label">{agent.display_name}</span>
        </div>
      )}
    </>
  );
}

// ─── ListRow ───────────────────────────────────────────────────────────────

interface ListRowProps {
  agent: AgentEntry;
  runtime: AgentRuntimeStatus | undefined;
  isSelected: boolean;
}

export function ListRow({ agent, runtime, isSelected }: ListRowProps) {
  const { t } = useT();
  const { setSelected } = useAgents();
  const color = CATEGORY_COLORS[agent.category] ?? "#6B7280";
  const pill = runtimePill(runtime?.state);
  const model =
    agent.model_id.length > 24 ? agent.model_id.slice(0, 24) + "…" : agent.model_id;
  return (
    <div
      className={`list-row${isSelected ? " list-row--selected" : ""}`}
      style={{ ["--cat" as string]: color }}
      data-agent={agent.name}
      onClick={() => setSelected(isSelected ? null : agent.name)}
    >
      <div className="list-row__main">
        <div className="list-avatar" style={{ background: color }}>
          {avatarInitials(agent.display_name)}
        </div>
        <span className="list-name">{agent.display_name}</span>
      </div>
      <span className="list-category">{agent.category}</span>
      <span className="list-model" title={agent.model_id}>
        {model}
      </span>
      <span className={pill.cls}>
        <span className="pill__dot" />
        {t(pill.key)}
      </span>
      <span className="list-row__open">{t("dashboard.open")} →</span>
    </div>
  );
}

// ─── Sidebar ───────────────────────────────────────────────────────────────

interface SidebarProps {
  activeCategory: string | null;
  agents: AgentEntry[];
  onSelect: (cat: string | null) => void;
}

export function Sidebar({ activeCategory, agents, onSelect }: SidebarProps) {
  const { t } = useT();
  const counts: Record<string, number> = {};
  for (const a of agents) counts[a.category] = (counts[a.category] ?? 0) + 1;

  return (
    <nav className="sidebar">
      <button
        className={`sidebar-item${activeCategory === null ? " sidebar-item--active" : ""}`}
        onClick={() => onSelect(null)}
      >
        {t("dashboard.all")} <span className="badge">{agents.length}</span>
      </button>
      {ALL_CATEGORIES.filter((c) => (counts[c] ?? 0) > 0).map((cat) => (
        <button
          key={cat}
          className={`sidebar-item${activeCategory === cat ? " sidebar-item--active" : ""}`}
          onClick={() => onSelect(cat)}
        >
          {cat} <span className="badge">{counts[cat]}</span>
        </button>
      ))}
    </nav>
  );
}

// ─── BrainBadge ────────────────────────────────────────────────────────────

function BrainBadge() {
  const { t } = useT();
  const [model, setModel] = useState<string | null>(null);
  useEffect(() => {
    invoke<[boolean, string | null]>("nudge_status").then(([, m]) => setModel(m)).catch(() => {});
  }, []);
  if (!model) return null;
  return (
    <button className="toolbar-btn brain-badge" title={t("dashboard.brainTooltip")}>
      🧠 {model}
    </button>
  );
}

// ─── DashboardApp ──────────────────────────────────────────────────────────

export function DashboardApp() {
  const { t, lang, setLang } = useT();
  const { agents, runtimeStatuses, selectedAgent, setSelected } = useAgents();
  const [activeCategory, setActiveCategory] = useState<string | null>(null);
  const [viewMode, setViewMode] = useState<"grid" | "list">("grid");
  const [query, setQuery] = useState("");
  const [wizardOpen, setWizardOpen] = useState(false);
  const [presetImportOpen, setPresetImportOpen] = useState(false);
  const [muragentImportOpen, setMuragentImportOpen] = useState(false);
  const [muragentImportPath, setMuragentImportPath] = useState<string | undefined>(undefined);
  const [showAppsBanner, setShowAppsBanner] = useState(false);
  const [showUpgradeNudge, setShowUpgradeNudge] = useState(false);
  const [nudgeDismissed, setNudgeDismissed] = useState(false);
  const searchRef = useRef<HTMLInputElement>(null);

  // Build a lookup map for runtime statuses.
  const runtimeMap = new Map<string, AgentRuntimeStatus>(
    runtimeStatuses.map((s) => [s.name, s]),
  );

  // Flock stats for the hero: count agents whose runtime is actively running.
  const runningCount = agents.filter(
    (a) => runtimeMap.get(a.name)?.state.state === "running",
  ).length;
  const idleCount = agents.length - runningCount;

  useEffect(() => {
    const unSelect = listen<string>("select-agent", (e) => {
      setTimeout(() => {
        document
          .querySelector(`[data-agent="${e.payload}"]`)
          ?.scrollIntoView({ behavior: "smooth", block: "center" });
      }, 50);
    });
    return () => {
      unSelect.then((fn) => fn());
    };
  }, []);

  // Listen for .muragent file open events from OS file association / deep-link
  useEffect(() => {
    const unlisten = listen<string>("open-muragent-file", (e) => {
      setMuragentImportPath(e.payload);
      setMuragentImportOpen(true);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // First-launch check: show banner if not running from /Applications
  useEffect(() => {
    invoke<{ is_first_launch: boolean; in_applications: boolean }>(
      "check_first_launch"
    ).then((status) => {
      if (!status.in_applications) {
        setShowAppsBanner(true);
      }
      if (status.is_first_launch) {
        invoke("mark_first_launch_done").catch(() => {});
      }
    }).catch(() => {});
  }, []);

  // Check nudge dismissal on mount.
  useEffect(() => {
    invoke<[boolean, string | null]>("nudge_status")
      .then(([dismissed]) => setNudgeDismissed(dismissed))
      .catch(() => {});
  }, []);



  // ⌘K focus search.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        searchRef.current?.focus();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const q = query.toLowerCase();
  const visible = agents.filter(
    (a) =>
      (activeCategory === null || a.category === activeCategory) &&
      (!q || a.name.toLowerCase().includes(q) || a.display_name.toLowerCase().includes(q)),
  );

  return (
    <div className="dashboard-root">
      <Sidebar activeCategory={activeCategory} agents={agents} onSelect={setActiveCategory} />
      <div className="dashboard-main dashboard">
        {showAppsBanner && (
          <div className="onboarding-banner">
            <span>
              {t("dashboard.moveToAppsBody", {
                folder: t("dashboard.applicationsFolder"),
              })
                .split(t("dashboard.applicationsFolder"))
                .flatMap((part, i) =>
                  i === 0
                    ? [part]
                    : [
                        <strong key={i}>{t("dashboard.applicationsFolder")}</strong>,
                        part,
                      ],
                )}
            </span>
            <button
              className="toolbar-btn"
              onClick={() => setShowAppsBanner(false)}
              title={t("dashboard.dismiss")}
            >
              ✕
            </button>
          </div>
        )}
        {showUpgradeNudge && !nudgeDismissed && (
          <div className="upgrade-nudge-banner">
            <span>{t("dashboard.nudgePrompt")}</span>
            <div className="upgrade-nudge-actions">
              <button
                className="toolbar-btn"
                onClick={() => {
                  invoke("nudge_dismiss").catch(() => {});
                  setNudgeDismissed(true);
                }}
              >
                {t("dashboard.nudgeDecline")}
              </button>
              <button
                className="toolbar-btn toolbar-btn--primary"
                onClick={() => {
                  setShowUpgradeNudge(false);
                  setWizardOpen(true);
                }}
              >
                {t("dashboard.nudgeAccept")}
              </button>
            </div>
          </div>
        )}

        <div className="dashboard__bar">
          <div className="dashboard__bar-actions">
            <button className="toolbar-btn" onClick={() => setWizardOpen(true)}>
              + {t("app.newAgent")}
            </button>
            <button
              className="toolbar-btn"
              onClick={() => setMuragentImportOpen(true)}
              title={t("app.importAgentTooltip")}
            >
              {t("app.importAgent")}
            </button>
            <button
              className="toolbar-btn"
              onClick={() => setPresetImportOpen(true)}
              title={t("app.importPresetTooltip")}
            >
              {t("app.importPreset")}
            </button>
            <BrainBadge />
            <label className="field dashboard__bar-search">
              <input
                ref={searchRef}
                type="search"
                placeholder={t("dashboard.search")}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
            </label>
          </div>
          <div className="dashboard__bar-right">
            <label className="lang-switch">
              {t("settings.language")}
              <select
                value={lang}
                onChange={(e) => setLang(e.target.value as "en" | "zh-TW")}
              >
                <option value="en">English</option>
                <option value="zh-TW">繁體中文</option>
              </select>
            </label>
            <div className="view-toggle">
              <button
                className={viewMode === "grid" ? "is-active" : ""}
                onClick={() => setViewMode("grid")}
                title={t("view.grid")}
                aria-label={t("view.grid")}
              >
                ⊞
              </button>
              <button
                className={viewMode === "list" ? "is-active" : ""}
                onClick={() => setViewMode("list")}
                title={t("view.list")}
                aria-label={t("view.list")}
              >
                ☰
              </button>
            </div>
            <button
              className="toolbar-btn"
              title={t("app.refresh")}
              aria-label={t("app.refresh")}
              onClick={() =>
                invoke<AgentEntry[]>("list_agents")
                  .then(() => {}) // state updated via event
                  .catch(console.error)
              }
            >
              ↺
            </button>
          </div>
        </div>

        <div className="dashboard__hero">
          <Mascot floating />
          <div>
            <h3>{t("dashboard.greeting", { name: "there" })}</h3>
            <p>
              {t("dashboard.flockStatus", {
                running: runningCount,
                idle: idleCount,
              })}
            </p>
          </div>
          <div className="dashboard__stats">
            <div className="stat">
              <div className="stat__n stat__n--run">{runningCount}</div>
              <div className="stat__l">{t("dashboard.stat.running")}</div>
            </div>
            <div className="stat">
              <div className="stat__n">{idleCount}</div>
              <div className="stat__l">{t("dashboard.stat.idle")}</div>
            </div>
          </div>
        </div>

        <div className="dashboard-content">
          {visible.length === 0 ? (
            <div className="empty-state">
              <Mascot floating size={96} />
              <h3>{t("dashboard.empty.title")}</h3>
              <p>{t("dashboard.empty.body")}</p>
              <button className="btn btn--primary" onClick={() => setWizardOpen(true)}>
                {t("dashboard.empty.cta")}
              </button>
            </div>
          ) : viewMode === "grid" ? (
            <div className="agent-grid">
              {visible.map((a) => (
                <GridCard
                  key={a.name}
                  agent={a}
                  runtime={runtimeMap.get(a.name)}
                  isSelected={selectedAgent === a.name}
                />
              ))}
            </div>
          ) : (
            <div className="agent-list">
              <div className="agent-list__head">
                <span>{t("dashboard.col.agent")}</span>
                <span>{t("dashboard.col.category")}</span>
                <span>{t("dashboard.col.model")}</span>
                <span>{t("dashboard.col.status")}</span>
                <span />
              </div>
              {visible.map((a) => (
                <ListRow
                  key={a.name}
                  agent={a}
                  runtime={runtimeMap.get(a.name)}
                  isSelected={selectedAgent === a.name}
                />
              ))}
            </div>
          )}
        </div>
      </div>

      {/* Detail panel — slides in when an agent is selected */}
      {selectedAgent && (
        <DetailPanel
          agentName={selectedAgent}
          agents={agents}
          onClose={() => setSelected(null)}
        />
      )}

      <WizardModal
        isOpen={wizardOpen}
        onClose={(name) => {
          setWizardOpen(false);
          if (name) invoke("list_agents").catch(() => {});
        }}
      />
      <PresetImportModal
        isOpen={presetImportOpen}
        onClose={() => setPresetImportOpen(false)}
      />
      <MuragentImportModal
        isOpen={muragentImportOpen}
        initialPath={muragentImportPath}
        onClose={() => {
          setMuragentImportOpen(false);
          setMuragentImportPath(undefined);
          invoke("list_agents").catch(() => {});
        }}
      />
    </div>
  );
}
