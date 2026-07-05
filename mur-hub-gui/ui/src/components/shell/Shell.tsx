import { useEffect, useState, type ReactNode } from "react";
import { Sidebar } from "./Sidebar";
import type { PageId } from "./nav";

// ⌘⌥I toggles the inspector column, independent of whether the caller
// currently provides an `inspector` node. No other modifiers may be held.
export function isInspectorToggle(e: KeyboardEvent): boolean {
  return (
    e.metaKey &&
    e.altKey &&
    !e.ctrlKey &&
    !e.shiftKey &&
    e.key.toLowerCase() === "i"
  );
}

export interface ShellProps {
  page: PageId;
  onNavigate: (id: PageId) => void;
  badge: number;
  inspector?: ReactNode;
  children: ReactNode;
}

export function Shell({ page, onNavigate, badge, inspector, children }: ShellProps) {
  const [inspectorVisible, setInspectorVisible] = useState(true);

  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (isInspectorToggle(e)) {
        e.preventDefault();
        setInspectorVisible((v) => !v);
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const showInspector = inspector !== undefined && inspectorVisible;

  return (
    <div className={`shell${showInspector ? " shell--with-inspector" : ""}`}>
      <div className="shell__sidebar">
        <Sidebar active={page} badge={badge} onSelect={onNavigate} />
      </div>
      <div className="shell__content">{children}</div>
      {showInspector && <div className="shell__inspector">{inspector}</div>}
    </div>
  );
}
