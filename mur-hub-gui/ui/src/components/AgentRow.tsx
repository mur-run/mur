import { invoke } from "@tauri-apps/api/core";
import type { AgentEntry } from "../types";

const CATEGORY_COLORS: Record<string, string> = {
  research: "#4F46E5",
  automation: "#059669",
  monitor: "#D97706",
  notify: "#DC2626",
  commerce: "#7C3AED",
  custom: "#6B7280",
};

interface AgentRowProps {
  agent: AgentEntry;
}

export function AgentRow({ agent }: AgentRowProps) {
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
        <span className="agent-row__category">{agent.category}</span>
      </div>
      <span className={`agent-row__status agent-row__status--${statusMod}`} />
    </button>
  );
}
