import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AgentEntry } from "../../types";
import type { ChannelSummary, ChannelEvent, Channel } from "../../work/types";
import { WorkChannelList } from "./WorkChannelList";
import { ChannelHitlBanner } from "./ChannelHitlBanner";
import { WorkFeed } from "./WorkFeed";
import { WorkTrace } from "./WorkTrace";

interface Props {
  agents: AgentEntry[];
}

/**
 * The Activity surface shows genuine multi-agent WORK — not 1:1 chats (those
 * live with each agent / in chat). A channel counts as "activity" if it has a
 * goal, more than one agent, or is a fleet channel.
 */
function isActivityChannel(ch: ChannelSummary): boolean {
  return (
    ch.goal.trim().length > 0 ||
    ch.agents.length > 1 ||
    ch.id.startsWith("fleet-")
  );
}

export function WorkView({ agents }: Props) {
  const [channels, setChannels] = useState<ChannelSummary[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [events, setEvents] = useState<ChannelEvent[]>([]);
  const [manifest, setManifest] = useState<Channel | null>(null);
  const [nowMs, setNowMs] = useState(Date.now());
  const selectedRef = useRef<string | null>(null);

  // Build agent-name → display-name lookup for rendering actor names.
  const displayNames: Record<string, string> = {};
  for (const a of agents) {
    displayNames[a.name] = a.display_name ?? a.name;
  }

  useEffect(() => {
    selectedRef.current = selectedId;
  }, [selectedId]);

  async function loadList() {
    const rows = await invoke<ChannelSummary[]>("channel_list");
    setChannels(rows);
    setNowMs(Date.now());
    // Auto-select the first ACTIVITY channel on first load.
    const activity = rows.filter(isActivityChannel);
    if (selectedRef.current === null && activity.length > 0) {
      setSelectedId(activity[0].id);
    }
  }

  async function loadSelected(id: string) {
    const [evs, mf] = await Promise.all([
      invoke<ChannelEvent[]>("channel_events", { channelId: id }),
      invoke<Channel>("channel_get", { channelId: id }),
    ]);
    // Guard stale responses — user may have switched away.
    if (selectedRef.current !== id) return;
    setEvents(evs);
    setManifest(mf);
  }

  // Initial load.
  useEffect(() => {
    void loadList();
  }, []);

  // Load detail whenever selection changes.
  useEffect(() => {
    if (selectedId) void loadSelected(selectedId);
    else {
      setEvents([]);
      setManifest(null);
    }
  }, [selectedId]);

  // Live update via file-watcher event.
  useEffect(() => {
    const un = listen<string>("channel-updated", (e) => {
      void loadList();
      if (selectedRef.current && e.payload === selectedRef.current) {
        void loadSelected(selectedRef.current);
      }
    });
    return () => {
      void un.then((f) => f());
    };
  }, []);

  // Tick the relative-time clock every 30 s.
  useEffect(() => {
    const id = setInterval(() => setNowMs(Date.now()), 30_000);
    return () => clearInterval(id);
  }, []);

  return (
    <div className="work-view">
      <div className="work-view__rail">
        <WorkChannelList
          channels={channels.filter(isActivityChannel)}
          selectedId={selectedId}
          nowMs={nowMs}
          onSelect={setSelectedId}
        />
      </div>
      <div className="work-view__feed">
        <ChannelHitlBanner
          channelId={selectedId}
          events={events}
          onResolved={() => {
            if (selectedId) void loadSelected(selectedId);
          }}
        />
        <WorkFeed events={events} displayNames={displayNames} nowMs={nowMs} />
      </div>
      <div className="work-view__trace">
        <WorkTrace channel={manifest} displayNames={displayNames} />
      </div>
    </div>
  );
}
