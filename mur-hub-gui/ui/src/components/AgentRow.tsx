import { invoke } from "@tauri-apps/api/core";
import type { AgentEntry } from "../types";
import { CATEGORY_COLORS } from "../utils";
import { useT } from "../i18n";
import type { TranslationKey } from "../i18n/types";

interface AgentRowProps {
  agent: AgentEntry;
}

export function AgentRow({ agent }: AgentRowProps) {
  const { t } = useT();
  const color = CATEGORY_COLORS[agent.category] ?? "#6B7280";
  const initials = agent.display_name
    .split(" ")
    .slice(0, 2)
    .map((w) => w[0]?.toUpperCase() ?? "")
    .join("");

  function handleClick() {
    invoke("open_dashboard", { agentName: agent.name }).catch(console.error);
  }

  const statusMod = agent.status === "running" ? "run" : "idle";

  return (
    <button className="agent-row" onClick={handleClick}>
      <div className="agent-row__avatar" style={{ background: color }}>
        {initials}
      </div>
      <div className="agent-row__info">
        <span className="agent-row__name">{agent.display_name}</span>
        <span className="agent-row__category">{t(`category.${agent.category}` as TranslationKey)}</span>
      </div>
      <span className={`agent-row__status agent-row__status--${statusMod}`} />
    </button>
  );
}
