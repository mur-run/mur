import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Channel } from "@tauri-apps/api/core";
import { useT } from "../i18n";

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
  const { t } = useT();
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
    return <div className="inbox-empty">{t("detail.loading")}</div>;
  }

  if (messages.length === 0) {
    return (
      <div className="inbox-empty">
        <p>{t("companion.empty")}</p>
        <p style={{ fontSize: 12, color: "var(--text-secondary)", marginTop: 4 }}>
          {t("companion.emptyHint")}
        </p>
      </div>
    );
  }

  function ackedLabel(msg: BridgeEvent): string {
    switch (msg.response.value) {
      case "good":
        return msg.situation === "workflow_nudge"
          ? t("companion.acked.saved")
          : t("companion.acked.good");
      case "bad":
        return t("companion.acked.bad");
      case "snooze":
        return t("companion.acked.snoozed");
      default:
        return t("companion.acked.dismissed");
    }
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
            {isUnread && msg.situation === "workflow_nudge" && (
              <div className="inbox-actions">
                <button className="btn btn--sm btn--primary" onClick={() => ack(msg.id, "good")}>{t("companion.save")}</button>
                <button className="btn btn--sm btn--secondary" onClick={() => ack(msg.id, "snooze")}>{t("companion.notNow")}</button>
                <button className="btn btn--sm btn--secondary" onClick={() => ack(msg.id, "dismiss")}>{t("companion.noThanks")}</button>
              </div>
            )}
            {isUnread && msg.situation !== "workflow_nudge" && (
              <div className="inbox-actions">
                <button className="btn btn--sm btn--primary" onClick={() => ack(msg.id, "good")} title={t("companion.good")}>👍</button>
                <button className="btn btn--sm btn--secondary" onClick={() => ack(msg.id, "bad")} title={t("companion.bad")}>👎</button>
                <button className="btn btn--sm btn--secondary" onClick={() => ack(msg.id, "dismiss")} title={t("companion.dismiss")}>🚫</button>
              </div>
            )}
            {!isUnread && (
              <span className="inbox-acked">{ackedLabel(msg)}</span>
            )}
          </li>
        );
      })}
    </ul>
  );
}

// ─── Unread badge hook ───────────────────────────────────────────────────────

export function useUnreadCount(agentName: string): number {
  const [count, setCount] = useState(0);

  useEffect(() => {
    invoke<number>("companion_unread_count", { agent: agentName })
      .then(setCount)
      .catch(() => {});
  }, [agentName]);

  return count;
}
