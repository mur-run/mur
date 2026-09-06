import type { AgentEntry, AgentRuntimeStatus } from "../../types";
import { Mascot } from "../Mascot";
import type { MascotMood } from "../Mascot";
import { useT } from "../../i18n";
import { timeGreetingKey } from "../../utils";
import { GridCard } from "./GridCard";

export interface AgentsOverviewProps {
  agents: AgentEntry[];
  runtimeMap: Map<string, AgentRuntimeStatus>;
  onNewAgent: () => void;
}

/** What the detail pane shows when nothing is selected (spec §3.4): the
 *  greeting + flock stats, then the pet-card grid at its designed size. */
export function AgentsOverview({ agents, runtimeMap, onNewAgent }: AgentsOverviewProps) {
  const { t } = useT();

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

  return (
    <div className="agents-overview">
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
        {agents.length === 0 ? (
          <div className="empty-state">
            <Mascot floating size={96} mood="excited" bubble={t("mascot.bubble.excited")} />
            <h3>{t("dashboard.empty.title")}</h3>
            <p>{t("dashboard.empty.body")}</p>
            <button className="btn btn--accent" onClick={onNewAgent}>
              {t("dashboard.empty.cta")}
            </button>
          </div>
        ) : (
          <div className="agent-grid">
            {agents.map((a) => (
              <GridCard key={a.name} agent={a} runtime={runtimeMap.get(a.name)} isSelected={false} />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
