import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ChannelSummary } from "../../work/types";

const CLOCK_TICK_MS = 30_000;

/**
 * The Home surface treats a channel as genuine multi-agent WORK — not a 1:1
 * chat — if it has a goal, more than one agent, or is a fleet channel. Moved
 * verbatim from the old WorkView so Now Running / Recent Activity keep the
 * exact same activity definition.
 */
export function isActivityChannel(ch: ChannelSummary): boolean {
  return (
    ch.goal.trim().length > 0 ||
    ch.agents.length > 1 ||
    ch.id.startsWith("fleet-")
  );
}

/** A channel is "running" if its state is not a terminal one. */
const TERMINAL_STATES = new Set([
  "completed",
  "failed",
  "canceled",
  "rejected",
]);

export function isRunningChannel(ch: ChannelSummary): boolean {
  return !TERMINAL_STATES.has(ch.state);
}

/**
 * Cross-agent channel list with live updates + a relative-time clock. This is
 * the data source that used to live inside WorkView (`channel_list` +
 * `channel-updated` watcher + 30s clock tick); Now Running and Recent Activity
 * both read from it.
 */
export function useChannels(): { channels: ChannelSummary[]; nowMs: number } {
  const [channels, setChannels] = useState<ChannelSummary[]>([]);
  const [nowMs, setNowMs] = useState(Date.now());
  const loadingRef = useRef(false);

  function loadList() {
    if (loadingRef.current) return;
    loadingRef.current = true;
    invoke<ChannelSummary[]>("channel_list")
      .then((rows) => {
        setChannels(rows);
        setNowMs(Date.now());
      })
      .catch(() => {})
      .finally(() => {
        loadingRef.current = false;
      });
  }

  useEffect(() => {
    loadList();
    const un = listen("channel-updated", () => loadList());
    const clock = setInterval(() => setNowMs(Date.now()), CLOCK_TICK_MS);
    return () => {
      un.then((f) => f()).catch(() => {});
      clearInterval(clock);
    };
  }, []);

  return { channels, nowMs };
}
