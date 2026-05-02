import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export function QuietHoursToggle({ agent }: { agent: string }) {
  const [busy, setBusy] = useState(false);
  async function quietFor1h() {
    setBusy(true);
    try {
      await invoke("companion_quiet", {
        agent,
        forSeconds: 3600,
        until: null,
        off: false,
      });
    } finally {
      setBusy(false);
    }
  }
  async function clearQuiet() {
    setBusy(true);
    try {
      await invoke("companion_quiet", {
        agent,
        forSeconds: null,
        until: null,
        off: true,
      });
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="flex gap-2 text-xs">
      <button disabled={busy} onClick={quietFor1h}>
        Quiet for 1h
      </button>
      <button disabled={busy} onClick={clearQuiet}>
        Clear quiet
      </button>
    </div>
  );
}
