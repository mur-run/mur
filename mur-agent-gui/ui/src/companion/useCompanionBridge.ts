import { useEffect, useState } from "react";
import {
  listPending,
  notifyMessage,
  setBadge,
  subscribe,
} from "./api";
import type { BridgeEvent } from "./types";

function unreadCount(events: BridgeEvent[]): number {
  return events.filter((m) => m.response.kind === "unset").length;
}

export function useCompanionBridge(agent: string | null) {
  const [messages, setMessages] = useState<BridgeEvent[]>([]);

  useEffect(() => {
    if (!agent) return;
    let cancelled = false;
    (async () => {
      const initial = await listPending(agent);
      if (cancelled) return;
      setMessages(initial);
      await setBadge(unreadCount(initial));
      await subscribe(agent, async (ev) => {
        if (cancelled) return;
        setMessages((prev) => {
          const dedup = prev.filter((m) => m.id !== ev.id);
          const next = [...dedup, ev].sort((a, b) =>
            a.generated_at.localeCompare(b.generated_at),
          );
          // Fire side-effects from inside the setState callback so we
          // see the post-merge array — avoids the stale-closure bug
          // flagged in plan §M5.5 self-review.
          void notifyMessage(ev.situation, ev.body);
          void setBadge(unreadCount(next));
          return next;
        });
      });
    })();
    return () => {
      cancelled = true;
    };
  }, [agent]);

  return { messages };
}
