import type { ChannelSummary } from "../../work/types";
import { relativeTime, stateBadge } from "../../work/format";
import { useT } from "../../i18n";

interface Props {
  channels: ChannelSummary[];
  selectedId: string | null;
  nowMs: number;
  onSelect: (id: string) => void;
}

export function WorkChannelList({
  channels,
  selectedId,
  nowMs,
  onSelect,
}: Props) {
  const { t } = useT();

  if (channels.length === 0) {
    return <div className="work-list work-list--empty">{t("work.empty")}</div>;
  }

  return (
    <div className="work-list">
      {channels.map((ch) => {
        const badge = stateBadge(ch.state);
        const isActive = ch.id === selectedId;
        return (
          <button
            key={ch.id}
            className={`work-list__item${isActive ? " is-active" : ""}`}
            onClick={() => onSelect(ch.id)}
          >
            <div className="work-list__top">
              <span className="work-list__title">
                {ch.title || ch.id.slice(0, 8)}
              </span>
              <span className={`work-badge work-badge--${badge}`}>
                {ch.state}
              </span>
            </div>
            <div className="work-list__sub">
              {ch.goal.trim() || ch.preview || ch.agents.join(", ")}
            </div>
            <div className="work-list__meta">
              {ch.turns} turn{ch.turns !== 1 ? "s" : ""} ·{" "}
              {relativeTime(ch.updated_at, nowMs)}
            </div>
          </button>
        );
      })}
    </div>
  );
}
