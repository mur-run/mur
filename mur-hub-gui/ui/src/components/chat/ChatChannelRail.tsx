import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ChannelSummary } from "../../work/types";

interface Props {
  agentName: string;
  activeId: string | null;
  onSelect: (id: string) => void;
}

function agentChannels(channels: ChannelSummary[], agentName: string): ChannelSummary[] {
  return channels.filter(
    (c) =>
      c.agents.includes(agentName) ||
      c.id === agentName ||
      c.id.startsWith(`${agentName}-`) ||
      c.id.startsWith(`fleet-`),
  );
}

function shortName(id: string): string {
  // "fleet-projectx" → "fleet/projectx", "rustsmith" → "main"
  if (id.startsWith("fleet-")) return id.slice(6);
  const parts = id.split("-");
  return parts.length > 1 ? parts.slice(1).join("-") : "main";
}

export function ChatChannelRail({ agentName, activeId, onSelect }: Props) {
  const [channels, setChannels] = useState<ChannelSummary[]>([]);

  async function load() {
    const all = await invoke<ChannelSummary[]>("channel_list").catch(() => []);
    setChannels(agentChannels(all, agentName));
  }

  useEffect(() => {
    void load();
    const un = listen("channel-updated", () => void load());
    return () => { void un.then((f) => f()); };
  }, [agentName]);

  const fleetChs = channels.filter((c) => c.id.startsWith("fleet-"));
  const agentChs = channels.filter((c) => !c.id.startsWith("fleet-"));

  function renderChannel(c: ChannelSummary) {
    const isActive = c.id === activeId;
    return (
      <button
        key={c.id}
        className={`cw-rail__channel${isActive ? " cw-rail__channel--active" : ""}`}
        onClick={() => onSelect(c.id)}
        title={c.title || c.id}
      >
        <span className="cw-rail__ch-hash">#</span>
        <span className="cw-rail__ch-name">{shortName(c.id)}</span>
        {c.turns > 0 && !isActive && (
          <span className="cw-rail__ch-badge">{c.turns > 99 ? "99+" : c.turns}</span>
        )}
      </button>
    );
  }

  if (channels.length === 0) {
    return (
      <div className="cw-rail">
        <div className="cw-rail__group-label">Channels</div>
        <div className="cw-rail__empty">
          <span className="cw-rail__empty-icon" />
          No channels yet
        </div>
      </div>
    );
  }

  return (
    <nav className="cw-rail">
      {agentChs.length > 0 && (
        <div className="cw-rail__section">
          <div className="cw-rail__group-label">Channels</div>
          {agentChs.map(renderChannel)}
        </div>
      )}
      {fleetChs.length > 0 && (
        <div className="cw-rail__section">
          <div className="cw-rail__group-label">Fleet</div>
          {fleetChs.map(renderChannel)}
        </div>
      )}
    </nav>
  );
}
