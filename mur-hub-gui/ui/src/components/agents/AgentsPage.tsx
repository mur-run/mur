import { useState } from "react";
import { useAgents } from "../../context/AgentContext";
import type { AgentEntry, AgentRuntimeStatus } from "../../types";
import { Mascot } from "../Mascot";
import type { MascotMood } from "../Mascot";
import { useT } from "../../i18n";
import { CATEGORY_COLORS, avatarInitials, runtimePill, timeGreetingKey } from "../../utils";
import { GridCard, Ico } from "./GridCard";

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
        {agent.role && <span className="role-chip">{agent.role}</span>}
      </div>
      <span className="list-category">{t(`category.${agent.category}` as Parameters<typeof t>[0])}</span>
      <span className="list-model" title={agent.model_id}>
        {model}
      </span>
      <span className={pill.cls}>
        <span className="pill__dot" />
        {t(pill.key)}
      </span>
      <button
        className="list-row__settings"
        onClick={(e) => { e.stopPropagation(); setSelected(isSelected ? null : agent.name); }}
        title={t("dashboard.settings")}
        aria-label={t("dashboard.settings")}
      >
        <Ico>
          <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
          <circle cx="12" cy="12" r="3" />
        </Ico>
      </button>
    </div>
  );
}

// ─── Sidebar (role filter) ───────────────────────────────────────────────────

/** Sentinel for the "no role assigned" sidebar bucket. */
export const NO_ROLE = "__none__";

interface SidebarProps {
  activeRole: string | null;
  agents: AgentEntry[];
  onSelect: (role: string | null) => void;
}

/**
 * Left rail filters by ROLE (was persona category, which was useless — nearly
 * every agent is "custom"). Lists each distinct role + a "no role" bucket.
 */
export function Sidebar({ activeRole, agents, onSelect }: SidebarProps) {
  const { t } = useT();
  const counts: Record<string, number> = {};
  let noRole = 0;
  for (const a of agents) {
    const r = a.role?.trim();
    if (r) counts[r] = (counts[r] ?? 0) + 1;
    else noRole++;
  }
  const roles = Object.keys(counts).sort((x, y) => x.localeCompare(y));

  return (
    <nav className="sidebar">
      <button
        className={`sidebar-item${activeRole === null ? " sidebar-item--active" : ""}`}
        onClick={() => onSelect(null)}
      >
        {t("dashboard.all")} <span className="badge">{agents.length}</span>
      </button>
      {roles.map((role) => (
        <button
          key={role}
          className={`sidebar-item${activeRole === role ? " sidebar-item--active" : ""}`}
          onClick={() => onSelect(role)}
        >
          <span className="sidebar-item__icon">🎭</span>
          {role} <span className="badge">{counts[role]}</span>
        </button>
      ))}
      {noRole > 0 && (
        <button
          className={`sidebar-item${activeRole === NO_ROLE ? " sidebar-item--active" : ""}`}
          onClick={() => onSelect(NO_ROLE)}
        >
          <span className="sidebar-item__icon">∅</span>
          {t("dashboard.noRole")} <span className="badge">{noRole}</span>
        </button>
      )}
    </nav>
  );
}

// ─── AgentsPage ──────────────────────────────────────────────────────────────

interface AgentsPageProps {
  agents: AgentEntry[];
  runtimeMap: Map<string, AgentRuntimeStatus>;
  query: string;
  viewMode: "grid" | "list";
  selectedAgent: string | null;
  onNewAgent: () => void;
}

export function AgentsPage({
  agents,
  runtimeMap,
  query,
  viewMode,
  selectedAgent,
  onNewAgent,
}: AgentsPageProps) {
  const { t } = useT();
  const [activeRole, setActiveRole] = useState<string | null>(null);

  // Flock stats for the hero: count agents whose runtime is actively running.
  const runningCount = agents.filter(
    (a) => runtimeMap.get(a.name)?.state.state === "running",
  ).length;
  const idleCount = agents.length - runningCount;

  const mascotMood: MascotMood =
    agents.length === 0
      ? "excited"
      : runningCount === agents.length
        ? "happy"
        : runningCount === 0
          ? "worried"
          : "idle";

  const mascotBubble =
    mascotMood === "excited"
      ? t("mascot.bubble.excited")
      : mascotMood === "happy"
        ? t("mascot.bubble.happy")
        : mascotMood === "worried"
          ? t("mascot.bubble.worried")
          : t("mascot.bubble.idle", { running: runningCount, idle: idleCount });

  const q = query.toLowerCase();
  const visible = agents.filter(
    (a) =>
      (activeRole === null ||
        (activeRole === NO_ROLE ? !a.role?.trim() : a.role?.trim() === activeRole)) &&
      (!q || a.name.toLowerCase().includes(q) || a.display_name.toLowerCase().includes(q)),
  );

  return (
    <div className="agents-view">
      <Sidebar activeRole={activeRole} agents={agents} onSelect={setActiveRole} />
      <div className="agents-view__content">
        <div className="dashboard__hero">
          <Mascot floating mood={mascotMood} bubble={mascotBubble} />
          <div>
            <h3>{t(timeGreetingKey())}</h3>
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
              <Mascot floating size={96} mood="excited" bubble={t("mascot.bubble.excited")} />
              <h3>{t("dashboard.empty.title")}</h3>
              <p>{t("dashboard.empty.body")}</p>
              <button className="btn btn--primary" onClick={onNewAgent}>
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
    </div>
  );
}
