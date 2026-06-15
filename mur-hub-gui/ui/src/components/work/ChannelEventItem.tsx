import type { ChannelEvent } from "../../work/types";
import { eventVariant, eventKindLabel, actorName } from "../../work/format";

interface Props {
  event: ChannelEvent;
  displayNames: Record<string, string>;
  nowMs: number;
}

export function ChannelEventItem({ event, displayNames, nowMs }: Props) {
  const variant = eventVariant(event.kind);
  const who = actorName(event.actor, displayNames);
  const text =
    typeof event.payload["text"] === "string" ? event.payload["text"] : "";

  const diffMs = nowMs - new Date(event.ts).getTime();
  const ts =
    diffMs < 60_000
      ? "just now"
      : diffMs < 3_600_000
        ? `${Math.floor(diffMs / 60_000)}m ago`
        : diffMs < 86_400_000
          ? `${Math.floor(diffMs / 3_600_000)}h ago`
          : `${Math.floor(diffMs / 86_400_000)}d ago`;

  if (variant === "message") {
    const role =
      event.actor.kind === "human"
        ? "user"
        : event.actor.kind === "agent"
          ? "agent"
          : "system";
    return (
      <div className={`work-event work-event--${role}`} data-seq={event.seq}>
        <div className="work-event__author">
          {who} <span className="work-event__ts">{ts}</span>
        </div>
        <div className="work-event__body">{text}</div>
      </div>
    );
  }

  if (variant === "note") {
    return (
      <div className="work-event work-event--note" data-seq={event.seq}>
        <div className="work-event__author">
          {who} <span className="work-event__ts">{ts}</span>
        </div>
        <div className="work-event__body work-event__body--note">{text}</div>
      </div>
    );
  }

  if (variant === "state") {
    const newState =
      typeof event.payload["new_state"] === "string"
        ? event.payload["new_state"]
        : event.kind;
    return (
      <div className="work-event work-event--state" data-seq={event.seq}>
        <span className="work-event__state-pill">{newState}</span>
        <span className="work-event__ts">{ts}</span>
      </div>
    );
  }

  // card: delegation, tool-call, hitl-request, etc.
  return (
    <div className="work-event work-event--card" data-seq={event.seq}>
      <div className="work-event__card-label">{eventKindLabel(event.kind)}</div>
      <div className="work-event__author">
        {who} <span className="work-event__ts">{ts}</span>
      </div>
    </div>
  );
}
