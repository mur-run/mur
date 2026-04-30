// Push-to-talk floating button. Default-off: renders null when
// voice_status.enabled === false (no nag), so users who skipped
// voice opt-in never see a "set up voice" floater on their screen.
//
// Hotkey wiring: the runtime emits `ptt://hotkey-down` /
// `ptt://hotkey-up` when the user presses Cmd+Shift+' (or rebound
// shortcut). Down → start capture. Up → stop, transcribe, hand
// transcript to onTranscript callback. Holds shorter than 250ms
// are treated as accidental and not transcribed; the FSM in the
// runtime side mirrors this with the same threshold.

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  sttTranscribePcm16k,
  voiceStartCapture,
  voiceStatus,
  voiceStopCapture,
} from "./api";

const MIN_HOLD_MS = 250;

interface Props {
  onTranscript: (transcript: string) => void;
}

export function PttButton({ onTranscript }: Props) {
  const [enabled, setEnabled] = useState(false);
  const [recording, setRecording] = useState(false);
  const [busy, setBusy] = useState(false);

  async function refresh() {
    try {
      const s = await voiceStatus();
      setEnabled(s.enabled && s.stt_loaded);
    } catch {
      setEnabled(false);
    }
  }

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
  }, []);

  useEffect(() => {
    if (!enabled) return;
    let downAt = 0;
    let unlistenDown: (() => void) | null = null;
    let unlistenUp: (() => void) | null = null;

    listen("ptt://hotkey-down", () => {
      downAt = Date.now();
      setRecording(true);
      voiceStartCapture().catch(() => {
        // Suppress errors here — already-running, no-device, etc.
        // The Up handler still tries to stop + drain.
      });
    }).then((u) => {
      unlistenDown = u;
    });

    listen("ptt://hotkey-up", async () => {
      const heldMs = Date.now() - downAt;
      setRecording(false);
      if (heldMs < MIN_HOLD_MS) {
        // Debounce — drop the capture without transcribing.
        try {
          await voiceStopCapture();
        } catch {
          /* ignore */
        }
        return;
      }
      setBusy(true);
      try {
        const samples = await voiceStopCapture();
        if (samples.length > 0) {
          const text = await sttTranscribePcm16k(samples);
          if (text) onTranscript(text);
        }
      } catch (e) {
        // Don't surface mid-flow errors as toasts; just log.
        // eslint-disable-next-line no-console
        console.warn("ptt transcribe failed", e);
      } finally {
        setBusy(false);
      }
    }).then((u) => {
      unlistenUp = u;
    });

    return () => {
      if (unlistenDown) unlistenDown();
      if (unlistenUp) unlistenUp();
    };
  }, [enabled, onTranscript]);

  // Default-off: render NOTHING when voice is disabled. Users who
  // skipped opt-in never see a floating "set up voice" prompt.
  if (!enabled) return null;

  return (
    <div
      className="fixed bottom-6 right-6 rounded-full px-4 py-3 shadow-lg text-sm font-medium select-none"
      style={{
        background: recording
          ? "var(--color-error, #b91c1c)"
          : busy
            ? "var(--color-bg-secondary)"
            : "var(--color-accent)",
        color: "var(--color-accent-fg)",
        opacity: busy ? 0.8 : 1,
        transition: "background 80ms ease",
      }}
      title={
        recording
          ? "Recording — release to transcribe"
          : busy
            ? "Transcribing…"
            : "Hold Cmd+Shift+' to talk"
      }
    >
      {recording ? "● recording" : busy ? "… transcribing" : "Cmd+Shift+'"}
    </div>
  );
}
