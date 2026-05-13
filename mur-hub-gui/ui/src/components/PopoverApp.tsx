import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAgents } from "../context/AgentContext";
import { AgentRow } from "./AgentRow";
import type { AgentEntry } from "../types";

const CATEGORY_ORDER = ["research", "automation", "monitor", "notify", "commerce", "custom"];

function groupByCategory(agents: AgentEntry[]) {
  const groups: Record<string, AgentEntry[]> = {};
  for (const agent of agents) {
    (groups[agent.category] ??= []).push(agent);
  }
  return groups;
}

export function PopoverApp() {
  const { agents } = useAgents();
  const [query, setQuery] = useState("");
  const searchRef = useRef<HTMLInputElement>(null);

  // Close on blur (lose focus).
  useEffect(() => {
    function onBlur() {
      invoke("toggle_popover").catch(console.error);
    }
    window.addEventListener("blur", onBlur);
    return () => window.removeEventListener("blur", onBlur);
  }, []);

  // ESC key → close.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") invoke("toggle_popover").catch(console.error);
      if ((e.metaKey || e.ctrlKey) && e.key === "f") {
        e.preventDefault();
        searchRef.current?.focus();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const q = query.toLowerCase();
  const filtered = agents.filter(
    (a) =>
      !q ||
      a.name.toLowerCase().includes(q) ||
      a.display_name.toLowerCase().includes(q),
  );
  const groups = groupByCategory(filtered);

  function showToast(msg: string) {
    // Minimal toast via alert fallback — no toast lib yet.
    const el = document.createElement("div");
    el.className = "toast";
    el.textContent = msg;
    document.body.appendChild(el);
    setTimeout(() => el.remove(), 2000);
  }

  return (
    <div className="popover-root">
      <div className="popover-search">
        <input
          ref={searchRef}
          type="search"
          placeholder="Search agents… (⌘F)"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          autoFocus
        />
      </div>

      <div className="popover-list">
        {filtered.length === 0 && (
          <p className="empty-hint">No agents found.</p>
        )}
        {CATEGORY_ORDER.filter((cat) => (groups[cat]?.length ?? 0) > 0).map((cat) => (
          <div key={cat} className="agent-group">
            <div className="group-header">{cat}</div>
            {groups[cat].map((agent) => (
              <AgentRow key={agent.name} agent={agent} />
            ))}
          </div>
        ))}
      </div>

      <div className="popover-footer">
        <button
          className="footer-btn primary"
          onClick={() =>
            window.open("https://docs.mur.run/agents/create", "_blank")
          }
        >
          + New Agent
        </button>
        <button className="footer-btn" onClick={() => showToast("Coming soon")}>
          ⚙ Settings
        </button>
        <button className="footer-btn" onClick={() => showToast("Coming soon")}>
          📥 Import
        </button>
      </div>
    </div>
  );
}
