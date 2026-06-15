import { useEffect, useRef } from "react";
import type { ChannelEvent } from "../../work/types";
import { ChannelEventItem } from "./ChannelEventItem";
import { useT } from "../../i18n";

interface Props {
  events: ChannelEvent[];
  displayNames: Record<string, string>;
  nowMs: number;
}

export function WorkFeed({ events, displayNames, nowMs }: Props) {
  const { t } = useT();
  const bottomRef = useRef<HTMLDivElement>(null);

  // Scroll to the newest event when the list changes.
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [events.length]);

  if (events.length === 0) {
    return (
      <div className="work-feed work-feed--empty">{t("work.noEvents")}</div>
    );
  }

  return (
    <div className="work-feed">
      {events.map((ev) => (
        <ChannelEventItem
          key={ev.seq}
          event={ev}
          displayNames={displayNames}
          nowMs={nowMs}
        />
      ))}
      <div ref={bottomRef} />
    </div>
  );
}
