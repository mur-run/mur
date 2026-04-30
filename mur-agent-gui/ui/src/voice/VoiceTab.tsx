// Two-state Voice tab. When voice_status.enabled === false, render
// only the opt-in panel. When enabled, render PrivacyBadge + picker
// + a "Disable voice" button.
//
// State source of truth: voice_status command + voice://state-changed
// event for live updates from voice_enable / voice_disable.

import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { voiceDisable, voiceStatus } from "./api";
import type { VoiceStatus } from "./types";
import { VoiceEnablePanel } from "./VoiceEnablePanel";
import { VoicePicker } from "./VoicePicker";
import { PrivacyBadge } from "./PrivacyBadge";

export function VoiceTab() {
  const [status, setStatus] = useState<VoiceStatus | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setStatus(await voiceStatus());
    } catch (e: unknown) {
      setErr(typeof e === "string" ? e : String(e));
    }
  }, []);

  useEffect(() => {
    refresh();
    let unlisten: (() => void) | null = null;
    listen("voice://state-changed", () => {
      refresh();
    }).then((u) => {
      unlisten = u;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [refresh]);

  async function handleDisable() {
    if (busy) return;
    setBusy(true);
    setErr(null);
    try {
      await voiceDisable();
      await refresh();
    } catch (e: unknown) {
      setErr(typeof e === "string" ? e : String(e));
    } finally {
      setBusy(false);
    }
  }

  if (err) {
    return (
      <div className="text-sm" style={{ color: "var(--color-error, #b91c1c)" }}>
        Voice tab error: {err}
      </div>
    );
  }
  if (!status) return <div className="text-sm opacity-60">Loading…</div>;

  if (!status.enabled) {
    return <VoiceEnablePanel onEnabled={refresh} />;
  }

  return (
    <div className="space-y-4">
      <PrivacyBadge />
      <VoicePicker />
      <div>
        <button
          onClick={handleDisable}
          disabled={busy}
          className="text-xs underline opacity-60 hover:opacity-100 transition-opacity"
          style={{ color: "var(--color-fg)" }}
        >
          Disable voice
        </button>
      </div>
    </div>
  );
}
