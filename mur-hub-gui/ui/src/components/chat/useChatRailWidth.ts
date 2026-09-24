import { useCallback, useEffect, useRef, useState } from "react";

/** Rail width bounds, in px. The floor matches `.cw-rail`'s `min-width` so CSS
 *  and JS cannot disagree about how narrow the rail may get; the ceiling keeps
 *  the chat column usable in the small chat window. */
export const RAIL_MIN_WIDTH = 160;
export const RAIL_MAX_WIDTH = 420;
/** Width before the user has ever dragged, and what a double-click restores. */
export const RAIL_DEFAULT_WIDTH = 200;
/** localStorage key. One width shared by every agent chat window: the rail is
 *  the same furniture in each, so per-agent widths would surprise. */
export const RAIL_WIDTH_KEY = "mur.chat.railWidth";

export function clampRailWidth(px: number): number {
  if (!Number.isFinite(px)) return RAIL_DEFAULT_WIDTH;
  return Math.min(RAIL_MAX_WIDTH, Math.max(RAIL_MIN_WIDTH, Math.round(px)));
}

/** Minimal slice of `Storage` the hook needs, so tests can pass a fake. */
export interface WidthStore {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

function defaultStore(): WidthStore | null {
  try {
    return window.localStorage;
  } catch {
    // Private mode / storage disabled: the rail still works, just unremembered.
    return null;
  }
}

/** Stored width, clamped. A missing, unparseable, or out-of-range value all
 *  land on a usable width rather than collapsing the rail. */
export function readRailWidth(store: WidthStore | null = defaultStore()): number {
  try {
    const raw = store?.getItem(RAIL_WIDTH_KEY) ?? null;
    if (raw === null) return RAIL_DEFAULT_WIDTH;
    return clampRailWidth(Number(raw));
  } catch {
    return RAIL_DEFAULT_WIDTH;
  }
}

export function writeRailWidth(px: number, store: WidthStore | null = defaultStore()): void {
  try {
    store?.setItem(RAIL_WIDTH_KEY, String(px));
  } catch {
    /* not worth surfacing — the drag already applied */
  }
}

export interface ChatRailWidth {
  width: number;
  /** Bind to the drag handle's `onPointerDown`. */
  onHandleDown: (e: React.PointerEvent<HTMLElement>) => void;
  /** Bind to the handle's `onDoubleClick` to restore the default width. */
  onHandleDoubleClick: () => void;
  dragging: boolean;
}

/**
 * Pointer-driven width for the chat channel rail, persisted to localStorage.
 *
 * Pointer capture (not window listeners) keeps the drag alive when the cursor
 * outruns the handle, and the write happens on pointer-up rather than on every
 * move so a drag is one storage write.
 */
export function useChatRailWidth(): ChatRailWidth {
  const [width, setWidth] = useState<number>(() => readRailWidth());
  const [dragging, setDragging] = useState(false);
  const startRef = useRef({ x: 0, w: RAIL_DEFAULT_WIDTH });
  const latest = useRef(width);
  latest.current = width;

  useEffect(() => {
    if (!dragging) writeRailWidth(width);
  }, [dragging, width]);

  const onHandleDown = useCallback((e: React.PointerEvent<HTMLElement>) => {
    e.preventDefault();
    startRef.current = { x: e.clientX, w: latest.current };
    setDragging(true);
    const el = e.currentTarget;
    el.setPointerCapture(e.pointerId);

    const move = (ev: PointerEvent) => {
      setWidth(clampRailWidth(startRef.current.w + (ev.clientX - startRef.current.x)));
    };
    const up = (ev: PointerEvent) => {
      el.releasePointerCapture?.(ev.pointerId);
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      el.removeEventListener("pointercancel", up);
      setDragging(false);
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  }, []);

  const onHandleDoubleClick = useCallback(() => setWidth(RAIL_DEFAULT_WIDTH), []);

  return { width, onHandleDown, onHandleDoubleClick, dragging };
}
