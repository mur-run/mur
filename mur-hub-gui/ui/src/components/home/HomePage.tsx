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
import { peekTargetForChannel, type PeekTarget } from "../peek/peekModel";

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
  /** Open the side-peek (spec 3(b)). */
  onPeek: (target: PeekTarget) => void;
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
  onPeek,
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

  const agentNames = new Set(agents.map((a) => a.name));
  // A channel row peeks its fleet or its agent's chat; other channels have no
  // viewer yet and keep going to the Chats page (spec 3(b) §4).
  function openChat(ch?: ChannelSummary) {
    const target = ch ? peekTargetForChannel(ch, agentNames) : null;
    if (target) onPeek(target);
    else onNavigate("chats");
  }
  const peekAgent = (name: string) => onPeek({ kind: "chat", agent: name });

  return (
    <div className="home-page">
      <NeedsYou items={items} onRefresh={onRefresh} onDismiss={onDismiss} onPeekAgent={peekAgent} />

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
            onOpen={openChat}
            onPeekAgent={peekAgent}
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
