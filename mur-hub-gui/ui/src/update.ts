import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

export async function checkForUpdates(): Promise<void> {
  try {
    const update = await check();
    if (!update) return;
    console.info(`MUR Hub update available: ${update.version}`);
    await update.downloadAndInstall();
    await relaunch();
  } catch (e) {
    // Non-fatal: Hub must still open even if update check fails
    console.warn("Update check failed:", e);
  }
}
