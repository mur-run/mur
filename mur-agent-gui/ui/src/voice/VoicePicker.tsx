// Voice picker — visible only when voice_status.enabled === true.
// Lists installed voices; clicking one sets it as default and plays
// a short preview ("Hi, I'm here to help.") via tts_speak.

import { useEffect, useState } from "react";
import { ttsSpeak, voiceListInstalled, voiceSetDefault } from "./api";
import type { VoiceListResponse } from "./types";

const PREVIEW_TEXT = "Hi, I'm here to help.";

export function VoicePicker() {
  const [data, setData] = useState<VoiceListResponse | null>(null);
  const [busy, setBusy] = useState(false);
  const [busyVoice, setBusyVoice] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  async function refresh() {
    try {
      const res = await voiceListInstalled();
      setData(res);
    } catch (e: unknown) {
      setErr(typeof e === "string" ? e : String(e));
    }
  }

  useEffect(() => {
    refresh();
  }, []);

  async function preview(voiceId: string) {
    if (busy) return;
    setBusy(true);
    setBusyVoice(voiceId);
    setErr(null);
    try {
      await voiceSetDefault(voiceId);
      await ttsSpeak(PREVIEW_TEXT);
      await refresh();
    } catch (e: unknown) {
      setErr(typeof e === "string" ? e : String(e));
    } finally {
      setBusy(false);
      setBusyVoice(null);
    }
  }

  if (err) {
    return (
      <div className="text-sm" style={{ color: "var(--color-error, #b91c1c)" }}>
        Voice picker error: {err}
      </div>
    );
  }
  if (!data) return <div className="text-sm opacity-60">Loading voices…</div>;
  if (data.voices.length === 0) {
    return (
      <div className="text-sm opacity-60">
        No voices installed yet. Add one with{" "}
        <code>mur agent companion voice download &lt;voice-id&gt;</code>{" "}
        or via the (forthcoming) Voice picker download button.
      </div>
    );
  }

  return (
    <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
      {data.voices.map((v) => {
        const isActive = data.default_voice_id === v.voice_id;
        const isBusy = busyVoice === v.voice_id;
        return (
          <button
            key={v.voice_id}
            disabled={busy}
            onClick={() => preview(v.voice_id)}
            className="rounded-md border p-3 text-left transition-colors"
            style={{
              borderColor: isActive
                ? "var(--color-accent)"
                : "var(--color-border)",
              background: isActive
                ? "var(--color-bg-secondary)"
                : "var(--color-bg)",
              opacity: busy && !isBusy ? 0.5 : 1,
            }}
          >
            <div
              className="font-medium"
              style={{ color: "var(--color-fg)" }}
            >
              {v.display_name}
              {isActive && (
                <span className="ml-2 text-xs font-normal opacity-70">
                  (active)
                </span>
              )}
              {isBusy && (
                <span className="ml-2 text-xs font-normal opacity-70">
                  playing…
                </span>
              )}
            </div>
            <div className="text-xs opacity-60 mt-1">
              {v.language} · {v.license} ·{" "}
              {(v.sample_rate_hz / 1000).toFixed(1)}&nbsp;kHz
            </div>
          </button>
        );
      })}
    </div>
  );
}
