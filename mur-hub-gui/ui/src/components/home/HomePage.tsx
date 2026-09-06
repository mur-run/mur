import type { AgentEntry, AgentRuntimeStatus } from "../../types";
import type { PageId } from "../shell/nav";
import type { InboxItem } from "./inbox";
import type { ChannelSummary } from "../../work/types";
import { useChannels, isActivityChannel, isRunningChannel } from "./useChannels";
import { NeedsYou } from "./NeedsYou";
import { NowRunning } from "./NowRunning";
import { RecentActivity } from "./RecentActivity";
import { Mascot } from "../Mascot";
import { useT } from "../../i18n";

interface Props {
  agents: AgentEntry[];
  runtimeStatuses: AgentRuntimeStatus[];
  /** Unified inbox items (owned by DashboardApp so the badge stays in sync). */
  items: InboxItem[];
  onRefresh: () => void;
  /** Dismiss an inbox item for this session (single source of truth lives in DashboardApp). */
  onDismiss: (item: InboxItem) => void;
  onNavigate: (id: PageId) => void;
  onCreateAgent: () => void;
}

/**
 * Mission-control Home. Composes the unified inbox (Needs you), live runs
 * (Now running) and recent cross-agent activity. When nothing is running and
 * there is no recent activity, shows the mascot with three quick actions.
 */
export function HomePage({
  agents,
  runtimeStatuses,
  items,
  onRefresh,
  onDismiss,
  onNavigate,
  onCreateAgent,
}: Props) {
  const { t } = useT();
  const { channels, nowMs } = useChannels();

  const hasRunningAgents = runtimeStatuses.some(
    (s) => s.state.state === "running",
  );
  const hasRunningChannels = channels
    .filter(isActivityChannel)
    .some(isRunningChannel);
  const hasRecent = channels.some(isActivityChannel);
  const showEmpty = !hasRunningAgents && !hasRunningChannels && !hasRecent;

  function openChat(_ch?: ChannelSummary) {
    onNavigate("chats");
  }

  return (
    <div className="home-page">
      <NeedsYou items={items} onRefresh={onRefresh} onDismiss={onDismiss} />

      {showEmpty ? (
        <div className="home-empty">
          <Mascot size={88} mood="idle" />
          <h2 className="home-empty__title">{t("home.empty.title")}</h2>
          <p className="home-empty__sub">{t("home.empty.subtitle")}</p>
          <div className="home-empty__actions">
            <button
              className="btn btn--accent"
              onClick={() => onNavigate("chats")}
            >
              {t("home.quick.newChat")}
            </button>
            <button
              className="btn btn--secondary"
              onClick={() => onNavigate("fleets")}
            >
              {t("home.quick.runFleet")}
            </button>
            <button className="btn btn--secondary" onClick={onCreateAgent}>
              {t("home.quick.createAgent")}
            </button>
          </div>
        </div>
      ) : (
        <>
          <NowRunning
            agents={agents}
            runtimeStatuses={runtimeStatuses}
            channels={channels}
            nowMs={nowMs}
            onOpen={() => openChat()}
          />
          <RecentActivity
            channels={channels}
            nowMs={nowMs}
            onOpen={openChat}
          />
        </>
      )}
    </div>
  );
}
