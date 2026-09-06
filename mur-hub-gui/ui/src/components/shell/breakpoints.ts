// Shell breakpoints (spec §3.1). These are the ONLY numbers that decide the
// layout; the CSS custom properties carry the matching widths.
export const BP_WIDE = 1200;
export const BP_COMPACT = 960;
export const SIDEBAR_PREF_KEY = "mur.shell.sidebar";

export type SidebarPref = "auto" | "expanded" | "collapsed";
export type SidebarMode = "expanded" | "collapsed";
export type ListMode = "wide" | "compact" | "overlay";

export function sidebarModeFor(width: number, pref: SidebarPref): SidebarMode {
  if (pref !== "auto") return pref;
  return width >= BP_WIDE ? "expanded" : "collapsed";
}

export function listModeFor(width: number): ListMode {
  if (width >= BP_WIDE) return "wide";
  if (width >= BP_COMPACT) return "compact";
  return "overlay";
}

/** ⌘\ toggles what is currently shown; a pin equal to the auto result
 *  collapses back to `auto` so a window resize takes over again. */
export function togglePref(current: SidebarPref, width: number): SidebarPref {
  const shown = sidebarModeFor(width, current);
  const next: SidebarMode = shown === "expanded" ? "collapsed" : "expanded";
  return next === sidebarModeFor(width, "auto") ? "auto" : next;
}

export function readSidebarPref(storage: Pick<Storage, "getItem">): SidebarPref {
  try {
    const v = storage.getItem(SIDEBAR_PREF_KEY);
    return v === "expanded" || v === "collapsed" ? v : "auto";
  } catch {
    return "auto";
  }
}

export function writeSidebarPref(storage: Pick<Storage, "setItem">, pref: SidebarPref): void {
  try {
    storage.setItem(SIDEBAR_PREF_KEY, pref);
  } catch {
    /* private mode / quota */
  }
}
