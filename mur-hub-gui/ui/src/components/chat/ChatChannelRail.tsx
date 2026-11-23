import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ChannelSummary } from "../../work/types";
import {
  channelTitle,
  displayNameMap,
  ordinalLabel,
  relativeTime,
} from "../../work/format";
import { useAgents } from "../../context/AgentContext";

interface Props {
  agentName: string;
  activeId: string | null;
  onSelect: (id: string) => void;
  /** Current rail width in px (user-draggable); omit for the CSS default. */
  width?: number;
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

/** What the rail prints for a channel: its title (the first message), else the
 *  preview line, else a legacy id's short name, else `{agent} · {date}` (an
 *  agent-opened channel has no human message yet) — never a uuid tail. */
export function channelLabel(
  c: ChannelSummary,
  displayNames: Record<string, string> = {},
): string {
  if (c.title.trim()) return c.title;
  if (c.preview) return c.preview;
  // Legacy name-keyed ids ("rustsmith", "fleet-x") carry their own label; only
  // a uuid id (every channel `create_for_agent` mints) would leak its tail.
  if (!UUID_RE.test(c.id)) return shortName(c.id);
  return channelTitle(c, displayNames);
}

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

function shortName(id: string): string {
  // "fleet-projectx" → "fleet/projectx", "rustsmith" → "main"
  if (id.startsWith("fleet-")) return id.slice(6);
  const parts = id.split("-");
  return parts.length > 1 ? parts.slice(1).join("-") : "main";
}

/** The id handle printed on a row. A uuid channel shows its handle. A
 *  name-keyed one (a run's `fleet-x`) shows none: its handle is the whole id,
 *  which the row's label already carries and the narrow rail has no room to
 *  repeat. {@link rowTooltip} keeps it readable. */
export function idChip(c: ChannelSummary): string {
  return UUID_RE.test(c.id) ? c.handle : "";
}

/** Hover text for a row: every handle that reaches it, then what it is. */
export function rowTooltip(c: ChannelSummary): string {
  const num = ordinalLabel(c.ordinal);
  return `${num ? `${num} · ` : ""}${c.handle} — ${c.title || c.id}`;
}

export function ChatChannelRail({ agentName, activeId, onSelect, width }: Props) {
  const { agents } = useAgents();
  const names = displayNameMap(agents);
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
  // No pick yet = the agent's latest channel, which is what ChatTab hydrates;
  // the list is newest-first, so highlight the first row rather than nothing.
  const highlighted = activeId ?? agentChs[0]?.id ?? null;
  const now = Date.now();

  function renderChannel(c: ChannelSummary) {
    const isActive = c.id === highlighted;
    const num = ordinalLabel(c.ordinal);
    const chip = idChip(c);
    return (
      <button
        key={c.id}
        className={`cw-rail__channel${isActive ? " cw-rail__channel--active" : ""}`}
        onClick={() => onSelect(c.id)}
        title={rowTooltip(c)}
      >
        <span className="cw-rail__ch-hash">{num || "#"}</span>
        <span className="cw-rail__ch-name">{channelLabel(c, names)}</span>
        {chip && <span className="cw-rail__ch-id">{chip}</span>}
        <span className="cw-rail__ch-time">{relativeTime(c.updated_at, now)}</span>
        {c.turns > 0 && !isActive && (
          <span className="cw-rail__ch-badge">{c.turns > 99 ? "99+" : c.turns}</span>
        )}
      </button>
    );
  }

  if (channels.length === 0) {
    return (
      <div className="cw-rail" style={width ? { width } : undefined}>
        <div className="cw-rail__group-label">Channels</div>
        <div className="cw-rail__empty">
          <span className="cw-rail__empty-icon" />
          No channels yet
        </div>
      </div>
    );
  }

  return (
    <nav className="cw-rail" style={width ? { width } : undefined}>
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
