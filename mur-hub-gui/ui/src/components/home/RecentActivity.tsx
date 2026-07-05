import type { ChannelSummary } from "../../work/types";
import { relativeTime, stateBadge } from "../../work/format";
import { isActivityChannel } from "./useChannels";
import { useT } from "../../i18n";

const MAX_ROWS = 8;

interface Props {
  channels: ChannelSummary[];
  nowMs: number;
  /** Jump to the chat for the clicked channel. */
  onOpen: (channel: ChannelSummary) => void;
}

/**
 * Recent cross-agent activity — the same `channel_list` activity channels
 * WorkView showed in its rail, ordered newest-first. Each row jumps to the
 * chat surface. Hidden when there is nothing.
 */
export function RecentActivity({ channels, nowMs, onOpen }: Props) {
  const { t } = useT();

  const rows = channels
    .filter(isActivityChannel)
    .slice()
    .sort((a, b) => b.updated_at.localeCompare(a.updated_at))
    .slice(0, MAX_ROWS);

  if (rows.length === 0) return null;

  return (
    <section className="home-section home-recent">
      <h2 className="home-section__title">{t("home.recentActivity")}</h2>
      <ul className="home-recent-list">
        {rows.map((ch) => (
          <li key={ch.id}>
            <button className="home-recent-row" onClick={() => onOpen(ch)}>
              <span className="home-recent-row__title">
                {ch.title || ch.id.slice(0, 8)}
              </span>
              <span className="home-recent-row__sub">
                {ch.goal.trim() || ch.preview || ch.agents.join(", ")}
              </span>
              <span className={`work-badge work-badge--${stateBadge(ch.state)}`}>
                {ch.state}
              </span>
              <span className="home-recent-row__meta">
                {relativeTime(ch.updated_at, nowMs)}
              </span>
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
