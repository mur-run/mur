import type { AgentDetail } from "../../../types";
import type { ChannelSummary } from "../../../work/types";
import type { AgentTabId } from "../../shell/detailTabs";
import { useT } from "../../../i18n";
import { NeedsYouBadge } from "../../shell/Status";
import { activityFor } from "./agentOverview";

export interface OverviewTabProps {
  detail: AgentDetail | null;
  channels: ChannelSummary[];
  agentName: string;
  needsYou: number;
  onGoTo: (tab: AgentTabId) => void;
  onOpenChat: () => void;
  onOpenHome: () => void;
}

/** Rendered where no Hub source exists yet (cost, turns today, schedule and
 *  memory counts) — spec §6.4 says show a dash, not invent a backend. */
const DASH = "—";

/** Overview lands first: what the agent is doing, then setup at a glance
 *  (spec §4.3). Activity comes from the same channel summaries Home reads. */
export function OverviewTab(p: OverviewTabProps) {
  const { t } = useT();
  const { now, recent } = activityFor(p.channels, p.agentName);
  const lastActive = recent[0] ? new Date(recent[0].updated_at).toLocaleTimeString() : DASH;
  return (
    <>
      {p.needsYou > 0 && (
        <div className="detail-attn" role="status">
          <NeedsYouBadge count={p.needsYou} />
          <span className="detail-attn__text">{t("status.needsYou", { count: p.needsYou })}</span>
          <button type="button" className="btn btn--secondary" onClick={p.onOpenHome}>
            {t("overview.review")}
          </button>
        </div>
      )}
      <div className="detail-card">
        <div className="detail-card__eyebrow">{t("overview.now")}</div>
        {now ? (
          <>
            <h4 className="overview-now__title">{now.title || now.goal}</h4>
            <p className="overview-now__sub">{now.preview}</p>
          </>
        ) : (
          <p className="overview-now__sub">{t("overview.nothingRunning")}</p>
        )}
        <button type="button" className="btn btn--link" onClick={p.onOpenChat}>
          {t("overview.openChat")}
        </button>
      </div>
      <div className="detail-card detail-stats">
        <div>
          <b>{DASH}</b>
          <span>{t("overview.costToday")}</span>
        </div>
        <div>
          <b>{DASH}</b>
          <span>{t("overview.turnsToday")}</span>
        </div>
        <div>
          <b>{recent.reduce((n, c) => n + c.turns, 0)}</b>
          <span>{t("overview.recentTurns")}</span>
        </div>
        <div>
          <b>{lastActive}</b>
          <span>{t("overview.lastActive")}</span>
        </div>
      </div>
      <div className="detail-two">
        <div className="detail-card">
          <div className="detail-card__eyebrow">{t("overview.recent")}</div>
          {recent.length === 0 && <p className="overview-now__sub">{t("overview.noRecent")}</p>}
          {recent.map((c) => (
            <div key={c.id} className="detail-kv">
              <span>{new Date(c.updated_at).toLocaleDateString()}</span>
              <span>{c.title || c.goal}</span>
              <span />
            </div>
          ))}
        </div>
        <div className="detail-card">
          <div className="detail-card__eyebrow">{t("overview.glance")}</div>
          <div className="detail-kv">
            <span>{t("detail.section.model")}</span>
            <span>{p.detail?.model_ref ?? DASH}</span>
            <button type="button" onClick={() => p.onGoTo("identity")}>{t("detail.tab.identity")}</button>
          </div>
          <div className="detail-kv">
            <span>{t("detail.skills")}</span>
            <span>{p.detail ? t("overview.count", { count: p.detail.skills.length }) : DASH}</span>
            <button type="button" onClick={() => p.onGoTo("capabilities")}>{t("detail.tab.capabilities")}</button>
          </div>
          <div className="detail-kv">
            <span>{t("detail.mcp")}</span>
            <span>{p.detail ? t("overview.count", { count: p.detail.mcp_servers.length }) : DASH}</span>
            <button type="button" onClick={() => p.onGoTo("capabilities")}>{t("detail.tab.capabilities")}</button>
          </div>
          <div className="detail-kv">
            <span>{t("detail.schedule")}</span>
            <span>{DASH}</span>
            <button type="button" onClick={() => p.onGoTo("automation")}>{t("detail.tab.automation")}</button>
          </div>
          <div className="detail-kv">
            <span>{t("detail.memory")}</span>
            <span>{DASH}</span>
            <button type="button" onClick={() => p.onGoTo("memory")}>{t("detail.tab.memory")}</button>
          </div>
        </div>
      </div>
    </>
  );
}
