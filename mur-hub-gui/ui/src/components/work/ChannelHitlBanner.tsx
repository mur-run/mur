import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ChannelEvent } from "../../work/types";
import { useT } from "../../i18n";

interface Props {
  channelId: string | null;
  events: ChannelEvent[];
  /** Called after a response is written so the parent can reload events. */
  onResolved: () => void;
}

interface PendingHitl {
  hitlId: string;
  summary: string;
  toolName: string;
  tier: string;
}

/** The latest hitl-request whose hitl_id has no later hitl-response. */
function findPendingHitl(events: ChannelEvent[]): PendingHitl | null {
  const responded = new Set<string>();
  for (const e of events) {
    if (e.kind === "hitl-response") {
      const id = e.payload["hitl_id"];
      if (typeof id === "string") responded.add(id);
    }
  }
  for (let i = events.length - 1; i >= 0; i--) {
    const e = events[i];
    if (e.kind !== "hitl-request") continue;
    const id = e.payload["hitl_id"];
    if (typeof id !== "string" || responded.has(id)) continue;
    const str = (k: string): string =>
      typeof e.payload[k] === "string" ? (e.payload[k] as string) : "";
    return {
      hitlId: id,
      summary: str("summary"),
      toolName: str("tool_name"),
      tier: str("tier"),
    };
  }
  return null;
}

/**
 * In the Activity panel: if the selected run has a pending risk-tiered HITL
 * gate, show approve/deny inline — pulling the CLI-only `mur channel approve`
 * into the GUI (writes a signed HitlResponse via channel_hitl_respond).
 */
export function ChannelHitlBanner({ channelId, events, onResolved }: Props) {
  const { t } = useT();
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const pending = findPendingHitl(events);
  if (!channelId || !pending) return null;

  async function respond(allow: boolean) {
    if (busy) return;
    setBusy(true);
    setErr(null);
    try {
      await invoke("channel_hitl_respond", {
        channelId,
        hitlId: pending!.hitlId,
        allow,
        reason: null,
      });
      onResolved();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="channel-hitl">
      <div className="channel-hitl__info">
        <span className="channel-hitl__badge">
          ⏸ {t("hitl.gate")}
          {pending.tier ? ` · ${pending.tier}` : ""}
        </span>
        <span className="channel-hitl__summary">
          {pending.summary || pending.toolName || pending.hitlId}
        </span>
      </div>
      <div className="channel-hitl__actions">
        <button
          className="hitl-card__btn hitl-card__btn--allow"
          onClick={() => respond(true)}
          disabled={busy}
        >
          ✅ {t("hitl.approve")}
        </button>
        <button
          className="hitl-card__btn hitl-card__btn--deny"
          onClick={() => respond(false)}
          disabled={busy}
        >
          ⛔ {t("hitl.deny")}
        </button>
      </div>
      {err && <p className="channel-hitl__err">{err}</p>}
    </div>
  );
}
