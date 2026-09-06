/** `#/detail/<kind>/<name>` — the hash `open_detail_window` loads (spec 2(b) §4). */
export type DetailKind = "agent" | "fleet";

export interface DetailRoute {
  kind: DetailKind;
  name: string;
}

export const DETAIL_HASH_PREFIX = "#/detail/";

/** Null for anything that is not a well-formed detail route; the window
 *  root then shows its "nothing to show" state instead of guessing. */
export function parseDetailRoute(hash: string): DetailRoute | null {
  if (!hash.startsWith(DETAIL_HASH_PREFIX)) return null;
  const rest = hash.slice(DETAIL_HASH_PREFIX.length);
  const slash = rest.indexOf("/");
  if (slash < 0) return null;
  const kind = rest.slice(0, slash);
  if (kind !== "agent" && kind !== "fleet") return null;
  let name: string;
  try {
    // Same encoding as `AgentChatWindow.agentNameFromHash`: `+` is a space.
    name = decodeURIComponent(rest.slice(slash + 1).replace(/\+/g, " "));
  } catch {
    return null;
  }
  return name ? { kind, name } : null;
}
