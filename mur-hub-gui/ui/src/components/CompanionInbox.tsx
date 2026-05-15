import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Channel } from "@tauri-apps/api/core";

interface BridgeResponse {
  kind: "unset" | "signal";
  value?: string;
}

interface BridgeEvent {
  id: string;
  situation: string;
  template_id: string;
  locale: string;
  generated_at: string;
  body: string;
  response: BridgeResponse;
}

interface Props {
  agentName: string;
}

export function CompanionInbox({ agentName }: Props) {
  const [messages, setMessages] = useState<BridgeEvent[]>([]);
  const [loading, setLoading] = useState(true);
  const channelRef = useRef<Channel<BridgeEvent> | null>(null);

  useEffect(() => {
    setLoading(true);

    // Load existing messages.
    invoke<BridgeEvent[]>("companion_bridge_pending", { agent: agentName })
      .then((evs) => {
        setMessages(evs);
        setLoading(false);
      })
      .catch(() => setLoading(false));

    // Subscribe for new messages.
    const ch = new Channel<BridgeEvent>();
    channelRef.current = ch;
    ch.onmessage = (ev) => {
      setMessages((prev) => {
        if (prev.some((m) => m.id === ev.id)) return prev;
        return [...prev, ev].sort((a, b) =>
          a.generated_at.localeCompare(b.generated_at),
        );
      });
    };
    invoke("companion_bridge_subscribe", {
      agent: agentName,
      onEvent: ch,
    }).catch(console.error);

    return () => {
      invoke("companion_bridge_unsubscribe", { agent: agentName }).catch(
        () => {},
      );
      channelRef.current = null;
    };
  }, [agentName]);

  async function ack(msgId: string, signal: string) {
    await invoke("companion_ack", { agent: agentName, msgId, signal }).catch(
      console.error,
    );
    setMessages((prev) =>
      prev.map((m) =>
        m.id === msgId
          ? { ...m, response: { kind: "signal", value: signal } }
          : m,
      ),
    );
  }

  if (loading) {
    return <div className="inbox-empty">Loading…</div>;
  }

  if (messages.length === 0) {
    return (
      <div className="inbox-empty">
        <p>No messages yet.</p>
        <p style={{ fontSize: 12, color: "var(--text-secondary, #888)", marginTop: 4 }}>
          The companion will appear here when the agent sends you a message.
        </p>
      </div>
    );
  }

  return (
    <ul className="inbox-list">
      {messages.map((msg) => {
        const isUnread = msg.response.kind === "unset";
        return (
          <li key={msg.id} className={`inbox-msg${isUnread ? " inbox-msg--unread" : ""}`}>
            <div className="inbox-msg-header">
              <span className="inbox-situation">{msg.situation}</span>
              <span className="inbox-time">
                {new Date(msg.generated_at).toLocaleTimeString([], {
                  hour: "2-digit",
                  minute: "2-digit",
                })}
              </span>
            </div>
            <p className="inbox-body">{msg.body}</p>
            {isUnread && (
              <div className="inbox-actions">
                <button onClick={() => ack(msg.id, "good")} title="Good">👍</button>
                <button onClick={() => ack(msg.id, "bad")} title="Bad">👎</button>
                <button onClick={() => ack(msg.id, "dismiss")} title="Dismiss">🚫</button>
              </div>
            )}
            {!isUnread && (
              <span className="inbox-acked">
                {msg.response.value === "good"
                  ? "👍 Acknowledged"
                  : msg.response.value === "bad"
                  ? "👎 Noted"
                  : "🚫 Dismissed"}
              </span>
            )}
          </li>
        );
      })}
    </ul>
  );
}

// ─── Unread badge hook ──────────────────────────────────────────────────────

export function useUnreadCount(agentName: string): number {
  const [count, setCount] = useState(0);

  useEffect(() => {
    invoke<number>("companion_unread_count", { agent: agentName })
      .then(setCount)
      .catch(() => {});
  }, [agentName]);

  return count;
}
