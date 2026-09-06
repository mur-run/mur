import { useEffect, useState } from "react";
import { readKey, writeKey } from "../../shell/persist";

export const INSTALL_TARGET_KEY = "mur.library.installTarget";

/** A stored agent that still exists wins; else the first agent; else empty. */
export function resolveInstallTarget(stored: string | null, agents: { name: string }[]): string {
  if (stored && agents.some((a) => a.name === stored)) return stored;
  return agents[0]?.name ?? "";
}

/** The agent Library installs go to, shared by all four pages and persisted. */
export function useInstallTarget(agents: { name: string }[]): [string, (name: string) => void] {
  const [target, setTarget] = useState(() => resolveInstallTarget(readKey(INSTALL_TARGET_KEY), agents));
  useEffect(() => {
    setTarget((t) => resolveInstallTarget(t || readKey(INSTALL_TARGET_KEY), agents));
  }, [agents]);
  function set(name: string) {
    setTarget(name);
    writeKey(INSTALL_TARGET_KEY, name);
  }
  return [target, set];
}
