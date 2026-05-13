import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AgentEntry } from "../types";

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

// ─── GridCard ──────────────────────────────────────────────────────────────

interface GridCardProps {
  agent: AgentEntry;
  isSelected: boolean;
}

export function GridCard({ agent, isSelected }: GridCardProps) {
  const color = CATEGORY_COLORS[agent.category] ?? "#6B7280";
  return (
    <div
      className={`grid-card${isSelected ? " grid-card--selected" : ""}`}
      data-agent={agent.name}
    >
      <div className="grid-avatar" style={{ background: color }}>
        {avatarInitials(agent.display_name)}
      </div>
      <p className="grid-name">{agent.display_name}</p>
      <div className="grid-status">
        <span className={`status-dot status-${agent.status}`} />
        <span className="grid-status-text">{agent.status}</span>
      </div>
      <div className="grid-actions">
        <button onClick={() => showToast("Coming in M-h2")}>Run</button>
        <button onClick={() => showToast("Coming in M-h2")}>Stop</button>
      </div>
    </div>
  );
}

// ─── ListRow ───────────────────────────────────────────────────────────────

interface ListRowProps {
  agent: AgentEntry;
  isSelected: boolean;
}

export function ListRow({ agent, isSelected }: ListRowProps) {
  const color = CATEGORY_COLORS[agent.category] ?? "#6B7280";
  const model =
    agent.model_id.length > 24 ? agent.model_id.slice(0, 24) + "…" : agent.model_id;
  return (
    <div
      className={`list-row${isSelected ? " list-row--selected" : ""}`}
      data-agent={agent.name}
    >
      <div className="list-avatar" style={{ background: color }}>
        {avatarInitials(agent.display_name)}
      </div>
      <span className="list-name">{agent.display_name}</span>
      <span className="list-category">{agent.category}</span>
      <span className="list-model" title={agent.model_id}>
        {model}
      </span>
      <span className={`status-dot status-${agent.status}`} />
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
  const counts: Record<string, number> = {};
  for (const a of agents) counts[a.category] = (counts[a.category] ?? 0) + 1;

  return (
    <nav className="sidebar">
      <button
        className={`sidebar-item${activeCategory === null ? " sidebar-item--active" : ""}`}
        onClick={() => onSelect(null)}
      >
        All <span className="badge">{agents.length}</span>
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

// ─── DashboardApp ──────────────────────────────────────────────────────────

export function DashboardApp() {
  const [agents, setAgents] = useState<AgentEntry[]>([]);
  const [selectedAgent, setSelectedAgent] = useState<string | null>(null);
  const [activeCategory, setActiveCategory] = useState<string | null>(null);
  const [viewMode, setViewMode] = useState<"grid" | "list">("grid");
  const [query, setQuery] = useState("");
  const searchRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    invoke<AgentEntry[]>("list_agents").then(setAgents).catch(console.error);

    const unAgents = listen<AgentEntry[]>("agents-updated", (e) => setAgents(e.payload));
    const unSelect = listen<string>("select-agent", (e) => {
      setSelectedAgent(e.payload);
      setTimeout(() => {
        document
          .querySelector(`[data-agent="${e.payload}"]`)
          ?.scrollIntoView({ behavior: "smooth", block: "center" });
      }, 50);
    });

    return () => {
      unAgents.then((fn) => fn());
      unSelect.then((fn) => fn());
    };
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
      <div className="dashboard-main">
        <div className="toolbar">
          <button
            className="toolbar-btn"
            onClick={() => window.open("https://docs.mur.run/agents/create", "_blank")}
          >
            + New Agent
          </button>
          <input
            ref={searchRef}
            type="search"
            className="toolbar-search"
            placeholder="Search… (⌘K)"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          <div className="view-toggle">
            <button
              className={viewMode === "grid" ? "active" : ""}
              onClick={() => setViewMode("grid")}
              title="Grid view"
            >
              ⊞
            </button>
            <button
              className={viewMode === "list" ? "active" : ""}
              onClick={() => setViewMode("list")}
              title="List view"
            >
              ☰
            </button>
          </div>
          <button
            className="toolbar-btn"
            onClick={() =>
              invoke<AgentEntry[]>("list_agents").then(setAgents).catch(console.error)
            }
          >
            ↺
          </button>
        </div>

        <div className="dashboard-content">
          {visible.length === 0 ? (
            <div className="empty-state">
              <div className="empty-illustration">◎</div>
              <p>No agents yet</p>
            </div>
          ) : viewMode === "grid" ? (
            <div className="grid-view">
              {visible.map((a) => (
                <GridCard key={a.name} agent={a} isSelected={selectedAgent === a.name} />
              ))}
            </div>
          ) : (
            <div className="list-view">
              {visible.map((a) => (
                <ListRow key={a.name} agent={a} isSelected={selectedAgent === a.name} />
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
