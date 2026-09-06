import { invoke } from "@tauri-apps/api/core";
import { showToast } from "../fleet/fleetActions";
import type { DetailKind } from "./detailRoute";

/** ⌘↩ on macOS, Ctrl+Enter elsewhere (spec 2(b) §7). */
export function isOpenInWindowShortcut(e: KeyboardEvent): boolean {
  return (e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key === "Enter";
}

/** True while a text field owns the keyboard, so page shortcuts stay out. */
export function isEditingTarget(el: Element | null): boolean {
  if (!el) return false;
  const tag = el.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || el.getAttribute("contenteditable") === "true";
}

const OPEN_ERROR_TOAST_MS = 4000;

/** Every trigger (⌘↩, ⋯, double-click, palette) goes through here. `title`
 *  is the display name; it becomes the window title. */
export async function openDetailWindow(kind: DetailKind, name: string, title: string): Promise<void> {
  try {
    await invoke("open_detail_window", { kind, name, title });
  } catch (err) {
    showToast(String(err), OPEN_ERROR_TOAST_MS);
  }
}
