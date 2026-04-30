// Default-off opt-in panel (per roadmap §4.1 + plan §M1.5.2).
// Renders when voice_status.enabled === false. The user reads what
// enabling will do, clicks "Enable voice", and watches the STT model
// download progress. On success, the parent flips to the enabled
// state which renders the picker + privacy badge instead.

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { voiceEnable } from "./api";
import type { DownloadProgress } from "./types";

interface Props {
  onEnabled: () => void;
}

type Phase = "idle" | "downloading" | "loading" | "error";

const STT_SIZE_HINT_MB = 809;
const ESTIMATED_MIN = 3;

export function VoiceEnablePanel({ onEnabled }: Props) {
  const [phase, setPhase] = useState<Phase>("idle");
  const [progress, setProgress] = useState({ done: 0, total: 0 });
  const [errMsg, setErrMsg] = useState<string | null>(null);

  useEffect(() => {
    if (phase !== "downloading") return;
    let unlisten: (() => void) | null = null;
    listen<DownloadProgress>("voice://stt-download-progress", (e) => {
      const p = e.payload;
      if (p.kind === "AssetProgress") {
        setProgress({ done: p.downloaded_bytes, total: p.total_bytes });
      } else if (p.kind === "Done") {
        setPhase("loading");
      }
    }).then((u) => {
      unlisten = u;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [phase]);

  async function start() {
    setErrMsg(null);
    setPhase("downloading");
    try {
      await voiceEnable();
      onEnabled();
    } catch (e: unknown) {
      setErrMsg(typeof e === "string" ? e : String(e));
      setPhase("error");
    }
  }

  const pct =
    progress.total > 0 ? Math.round((progress.done / progress.total) * 100) : 0;
  const fmtMb = (n: number) => (n / 1_000_000).toFixed(0);

  return (
    <div
      className="rounded-lg border p-5 max-w-2xl"
      style={{
        borderColor: "var(--color-border)",
        background: "var(--color-bg-secondary)",
      }}
    >
      <div className="flex items-center gap-2 mb-3">
        <span className="text-xs uppercase tracking-wider opacity-60">
          Voice
        </span>
        <span
          className="text-xs px-2 py-0.5 rounded"
          style={{
            background: "var(--color-bg)",
            color: "var(--color-fg-muted, var(--color-fg))",
          }}
        >
          DISABLED
        </span>
        <span className="text-xs opacity-60 ml-auto">
          Privacy: voice never leaves this Mac
        </span>
      </div>

      <h2
        className="text-lg font-medium mb-2"
        style={{ color: "var(--color-fg)" }}
      >
        Speak and listen, fully on-device
      </h2>
      <p className="text-sm mb-3 opacity-80">
        mur agents can speak and listen using fully on-device speech
        models. Nothing is uploaded to a server.
      </p>
      <p className="text-sm opacity-80 mb-2">Enabling will:</p>
      <ul className="text-sm opacity-80 mb-4 list-disc pl-5 space-y-1">
        <li>
          Download the speech model (~{STT_SIZE_HINT_MB}&nbsp;MB, one-time;
          about {ESTIMATED_MIN}&nbsp;min on broadband)
        </li>
        <li>
          Bundle 1 default voice; you can add 4 more on demand from
          Settings
        </li>
        <li>
          Register <code>Cmd+Shift+'</code> as the push-to-talk hotkey
          (rebindable)
        </li>
        <li>Request microphone permission from your OS on first use</li>
      </ul>

      {phase === "idle" && (
        <div className="flex gap-3">
          <button
            onClick={start}
            className="px-4 py-2 text-sm rounded font-medium"
            style={{
              background: "var(--color-accent)",
              color: "var(--color-accent-fg)",
            }}
          >
            Enable voice
          </button>
        </div>
      )}

      {phase === "downloading" && (
        <>
          <div className="text-sm mb-2">Downloading speech model…</div>
          <div
            className="h-2 rounded overflow-hidden mb-2"
            style={{ background: "var(--color-bg)" }}
          >
            <div
              className="h-full transition-all"
              style={{
                width: `${pct}%`,
                background: "var(--color-accent)",
              }}
            />
          </div>
          <div className="text-xs opacity-70">
            {fmtMb(progress.done)} / {fmtMb(progress.total)}&nbsp;MB ({pct}%)
          </div>
        </>
      )}

      {phase === "loading" && (
        <div className="text-sm opacity-80">Loading model… nearly done.</div>
      )}

      {phase === "error" && (
        <>
          <div className="text-sm mb-2" style={{ color: "var(--color-error, #b91c1c)" }}>
            Enable failed: {errMsg ?? "unknown error"}
          </div>
          <button
            onClick={start}
            className="px-4 py-2 text-sm rounded"
            style={{
              background: "var(--color-accent)",
              color: "var(--color-accent-fg)",
            }}
          >
            Try again
          </button>
        </>
      )}
    </div>
  );
}
