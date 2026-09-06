import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { confirm } from "@tauri-apps/plugin-dialog";

interface DirtyCtx {
  dirty: ReadonlySet<string>;
  mark: (id: string, isDirty: boolean) => void;
}

const Ctx = createContext<DirtyCtx>({ dirty: new Set(), mark: () => {} });

/** Wrap one master–detail page. Sections report unsaved edits with
 *  useMarkDirty; the list and tab bar ask useDirtyGuard before leaving. */
export function DirtyProvider({ children }: { children: ReactNode }) {
  const [dirty, setDirty] = useState<Set<string>>(() => new Set());
  const mark = useCallback((id: string, isDirty: boolean) => {
    setDirty((prev) => {
      if (prev.has(id) === isDirty) return prev;
      const next = new Set(prev);
      if (isDirty) next.add(id);
      else next.delete(id);
      return next;
    });
  }, []);
  const value = useMemo(() => ({ dirty, mark }), [dirty, mark]);
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

/** `useMarkDirty("persona", form differs from saved)`. Clears on unmount. */
export function useMarkDirty(id: string, isDirty: boolean): void {
  const { mark } = useContext(Ctx);
  useEffect(() => {
    mark(id, isDirty);
  }, [id, isDirty, mark]);
  useEffect(() => () => mark(id, false), [id, mark]);
}

export function shouldConfirmLeave(dirty: ReadonlySet<string>): boolean {
  return dirty.size > 0;
}

export function useDirtyGuard(): {
  isDirty: boolean;
  /** Resolves true when leaving is fine: nothing dirty, or the user chose to discard. */
  confirmLeave: (message: string, title: string) => Promise<boolean>;
} {
  const { dirty } = useContext(Ctx);
  const confirmLeave = useCallback(
    async (message: string, title: string) => {
      if (!shouldConfirmLeave(dirty)) return true;
      return confirm(message, { title, kind: "warning" });
    },
    [dirty],
  );
  return { isDirty: shouldConfirmLeave(dirty), confirmLeave };
}
