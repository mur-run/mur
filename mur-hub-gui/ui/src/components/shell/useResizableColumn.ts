import { useCallback, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { readKey, writeKey } from "./persist";

/** List-pane widths (px). Mirror --shell-list-width / -min / -max in primitives.css. */
export const LIST_WIDTH_DEFAULT = 300;
export const LIST_WIDTH_MIN = 240;
export const LIST_WIDTH_MAX = 400;

export function clampWidth(w: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, Math.round(w)));
}

export function parseStoredWidth(raw: string | null, fallback: number, min: number, max: number): number {
  const n = raw === null ? Number.NaN : Number(raw);
  return Number.isFinite(n) ? clampWidth(n, min, max) : fallback;
}

export interface ResizableColumn {
  width: number;
  onPointerDown: (e: ReactPointerEvent<HTMLElement>) => void;
  /** Double-click on the divider: back to the default width. */
  reset: () => void;
}

/** A pointer-dragged column width, clamped to [min, max] and persisted under
 *  `storageKey` on release (spec §3.1). */
export function useResizableColumn(storageKey: string, fallback: number, min: number, max: number): ResizableColumn {
  const [width, setWidth] = useState(() => parseStoredWidth(readKey(storageKey), fallback, min, max));
  const drag = useRef<{ startX: number; startW: number } | null>(null);

  const onPointerDown = useCallback(
    (e: ReactPointerEvent<HTMLElement>) => {
      if (e.button !== 0) return;
      e.preventDefault();
      const target = e.currentTarget;
      target.setPointerCapture(e.pointerId);
      drag.current = { startX: e.clientX, startW: width };
      function onMove(ev: PointerEvent) {
        if (!drag.current) return;
        setWidth(clampWidth(drag.current.startW + (ev.clientX - drag.current.startX), min, max));
      }
      function onUp() {
        drag.current = null;
        target.removeEventListener("pointermove", onMove);
        target.removeEventListener("pointerup", onUp);
        setWidth((w) => {
          writeKey(storageKey, String(w));
          return w;
        });
      }
      target.addEventListener("pointermove", onMove);
      target.addEventListener("pointerup", onUp);
    },
    [width, min, max, storageKey],
  );

  const reset = useCallback(() => {
    setWidth(fallback);
    writeKey(storageKey, null);
  }, [fallback, storageKey]);

  return { width, onPointerDown, reset };
}
