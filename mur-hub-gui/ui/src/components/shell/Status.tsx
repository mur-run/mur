import { useT } from "../../i18n";
import type { TranslationKey } from "../../i18n/types";
import type { RuntimeState } from "../../types";

/** The only status vocabulary in the Hub (spec §4.5). Amber on a BADGE means
 *  "needs you"; amber on a PILL means restarting. They never share a shape. */
export type StatusKind = "running" | "idle" | "restarting" | "stopped" | "failed";

/** Agent runtime → kind. A stopped agent is the ordinary not-running state and
 *  reads as idle; the red `stopped` kind is reserved for a fleet kill-switch. */
export function statusOf(rt: RuntimeState | undefined): StatusKind {
  switch (rt?.state) {
    case "running":
      return "running";
    case "restarting":
      return "restarting";
    case "failed":
      return "failed";
    default:
      return "idle";
  }
}

export function fleetStatusOf(f: { stopped: boolean; running: boolean }): StatusKind {
  if (f.stopped) return "stopped";
  return f.running ? "running" : "idle";
}

const LABEL_KEY: Record<StatusKind, TranslationKey> = {
  running: "status.running",
  idle: "status.idle",
  restarting: "status.restarting",
  stopped: "status.stopped",
  failed: "status.failed",
};

export function StatusDot({ kind, title }: { kind: StatusKind; title?: string }) {
  return (
    <span
      className={`status-dot status-dot--${kind}`}
      title={title}
      role={title ? "img" : undefined}
      aria-label={title}
      aria-hidden={title ? undefined : true}
    />
  );
}

export function StatusPill({ kind }: { kind: StatusKind }) {
  const { t } = useT();
  return (
    <span className={`status-pill status-pill--${kind}`}>
      <span className="status-pill__dot" aria-hidden="true" />
      {t(LABEL_KEY[kind])}
    </span>
  );
}

const BADGE_CAP = 99;

export function NeedsYouBadge({ count, title }: { count: number; title?: string }) {
  if (count <= 0) return null;
  return (
    <span className="needs-you" title={title} aria-label={title}>
      {count > BADGE_CAP ? `${BADGE_CAP}+` : count}
    </span>
  );
}
