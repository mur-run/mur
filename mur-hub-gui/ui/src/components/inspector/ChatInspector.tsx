//! Chat inspector — contextual right-pane for the Chats page. Shows the active
//! chat's model, channel id, and turn count. Model comes from `get_agent_detail`;
//! the channel from `channel_list` (the agent's own primary channel — id equals
//! the agent name).

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AgentDetail } from "../../types";
import type { ChannelSummary } from "../../work/types";
import { useT } from "../../i18n";

interface Props {
  agentName: string;
  displayName?: string;
  onClose: () => void;
}

export function ChatInspector({ agentName, displayName, onClose }: Props) {
  const { t } = useT();
  const [detail, setDetail] = useState<AgentDetail | null>(null);
  const [channel, setChannel] = useState<ChannelSummary | null>(null);

  useEffect(() => {
    let cancelled = false;
    invoke<AgentDetail>("get_agent_detail", { name: agentName })
      .then((d) => { if (!cancelled) setDetail(d); })
      .catch(() => { if (!cancelled) setDetail(null); });
    invoke<ChannelSummary[]>("channel_list")
      .then((all) => {
        if (cancelled) return;
        // The agent's own conversation channel is keyed by its name.
        const own = all.find((c) => c.id === agentName)
          ?? all.find((c) => c.agents.includes(agentName));
        setChannel(own ?? null);
      })
      .catch(() => { if (!cancelled) setChannel(null); });
    return () => { cancelled = true; };
  }, [agentName]);

  const model = detail
    ? `${detail.model_provider}${detail.model_name ? ` · ${detail.model_name}` : ""}`
    : agentName;

  return (
    <aside className="detail-panel detail-panel--inspector">
      <div className="detail-panel__header">
        <div className="detail-panel__top">
          <div className="detail-panel__ident">
            <div className="detail-panel__name">{displayName ?? agentName}</div>
            <span className="field-muted" style={{ fontSize: 12 }}>{t("chatInspector.subtitle")}</span>
          </div>
          <button
            className="detail-panel__close"
            onClick={onClose}
            title={t("detail.close")}
            aria-label={t("detail.close")}
          >
            ×
          </button>
        </div>
      </div>
      <div className="detail-panel__body">
        <div className="tab-form">
          <label className="field-label">{t("chatInspector.model")}</label>
          <p className="field-value">{model}</p>
          {detail?.model_ref && (
            <code className="item-card-code">{detail.model_ref}</code>
          )}

          <label className="field-label" style={{ marginTop: 12 }}>{t("chatInspector.channel")}</label>
          {channel ? (
            <>
              <code className="item-card-code">{channel.id}</code>
              <p className="field-muted" style={{ fontSize: 12 }}>
                {t("chatInspector.turns", { count: channel.turns })}
              </p>
            </>
          ) : (
            <p className="field-muted" style={{ fontSize: 12 }}>{t("chatInspector.noChannel")}</p>
          )}
        </div>
      </div>
    </aside>
  );
}
