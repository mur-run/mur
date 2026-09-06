import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { confirm } from "@tauri-apps/plugin-dialog";
import type { FleetDetail as Detail } from "../../fleet/types";

export function showToast(msg: string, durationMs = 2500) {
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = msg;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), durationMs);
}

/** The `busy` + `call()` pair every fleet tab used from the old FleetDetail. */
export function useFleetCall(onRefresh: () => void): {
  busy: string | null;
  setBusy: (v: string | null) => void;
  call: (cmd: string, args: Record<string, unknown>) => Promise<void>;
} {
  const [busy, setBusy] = useState<string | null>(null);
  async function call(cmd: string, args: Record<string, unknown>) {
    setBusy(cmd);
    try {
      await invoke(cmd, args);
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }
  return { busy, setBusy, call };
}

/** Confirm-then-delete, shared by the header's ⋯ menu and the Settings danger zone. */
export async function deleteFleet(
  detail: Detail,
  labels: { confirm: string; title: string },
  setBusy: (v: string | null) => void,
  onDelete: () => void,
): Promise<void> {
  const ok = await confirm(labels.confirm, { title: labels.title, kind: "warning" });
  if (!ok) return;
  setBusy("fleet_delete");
  try {
    await invoke("fleet_delete", { name: detail.name });
    onDelete();
  } catch (err) {
    showToast(String(err), 4000);
    setBusy(null);
  }
}
