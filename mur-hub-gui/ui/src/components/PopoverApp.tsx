import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAgents } from "../context/AgentContext";
import { useT } from "../i18n";
import { AgentRow } from "./AgentRow";
import { Mascot } from "./Mascot";
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
  const { t } = useT();
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

  function openCreate() {
    window.open("https://docs.mur.run/agents/create", "_blank");
  }

  // First-run empty state: no agents at all.
  if (agents.length === 0) {
    return (
      <div className="popover">
        <div className="popover__empty">
          <div className="empty-state">
            <Mascot floating size={72} />
            <h3>{t("popover.empty.title")}</h3>
            <button className="btn btn--primary" onClick={openCreate}>
              {t("popover.empty.cta")}
            </button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="popover">
      <div className="popover__search field">
        <input
          ref={searchRef}
          type="search"
          placeholder={t("popover.search")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          autoFocus
        />
      </div>

      <div className="popover__list">
        {filtered.length === 0 && (
          <p className="popover__empty-hint">{t("popover.noneFound")}</p>
        )}
        {CATEGORY_ORDER.filter((cat) => (groups[cat]?.length ?? 0) > 0).map((cat) => (
          <div key={cat} className="agent-group">
            <div className="agent-group__header">{cat}</div>
            {groups[cat].map((agent) => (
              <AgentRow key={agent.name} agent={agent} />
            ))}
          </div>
        ))}
      </div>

      <div className="popover__footer">
        <button className="btn btn--primary" onClick={openCreate}>
          {t("app.newAgent")}
        </button>
        <button
          className="btn btn--secondary"
          onClick={() => invoke("open_dashboard", {}).catch(console.error)}
        >
          {t("popover.openHub")}
        </button>
      </div>
    </div>
  );
}
