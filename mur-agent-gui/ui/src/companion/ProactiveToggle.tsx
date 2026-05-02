import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export function ProactiveToggle({
  agent,
  initialEnabled,
}: {
  agent: string;
  initialEnabled: boolean;
}) {
  const [enabled, setEnabled] = useState(initialEnabled);
  async function toggle() {
    const next = !enabled;
    setEnabled(next);
    try {
      await invoke("companion_proactive", { agent, enabled: next });
    } catch (e) {
      console.warn("companion_proactive failed:", e);
      setEnabled(!next);
    }
  }
  return (
    <label className="flex items-center gap-2 text-xs">
      <input type="checkbox" checked={enabled} onChange={toggle} />
      Proactive messages
    </label>
  );
}
