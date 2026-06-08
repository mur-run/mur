// Fire a toast notification for a background HITL event on a non-active agent.
// Mirrors the showToast pattern in DashboardApp (DOM-only, no Tauri notification
// plugin needed) so we stay consistent with the Phase-2 notification approach.
export function notifyHitlBackground(displayName: string) {
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = `⏳ ${displayName} needs approval`;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), 4000);
}
