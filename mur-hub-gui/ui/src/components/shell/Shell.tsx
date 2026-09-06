import { useEffect, useState, type ReactNode } from "react";
import { Sidebar } from "./Sidebar";
import type { PageId } from "./nav";
import {
  readSidebarPref, sidebarModeFor, togglePref, writeSidebarPref, type SidebarPref,
} from "./breakpoints";
import { useWindowWidth } from "./useWindowWidth";
import { isMac } from "./platform";

/** ⌘\ toggles the sidebar between labels and the icon rail (spec §3.1). */
export function isSidebarToggle(e: KeyboardEvent): boolean {
  return e.metaKey && !e.altKey && !e.ctrlKey && !e.shiftKey && e.key === "\\";
}

function initialPref(): SidebarPref {
  return typeof localStorage === "undefined" ? "auto" : readSidebarPref(localStorage);
}

export interface ShellProps {
  page: PageId;
  onNavigate: (id: PageId) => void;
  badge: number;
  /** Banners render at the top of the content column, never above the sidebar. */
  banners?: ReactNode;
  onSettings: () => void;
  onSearch: () => void;
  children: ReactNode;
}

export function Shell({ page, onNavigate, badge, banners, onSettings, onSearch, children }: ShellProps) {
  const [pref, setPref] = useState<SidebarPref>(initialPref);
  const width = useWindowWidth();
  const sidebarMode = sidebarModeFor(width, pref);

  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (isSidebarToggle(e)) {
        e.preventDefault();
        setPref((p) => {
          const next = togglePref(p, window.innerWidth);
          writeSidebarPref(localStorage, next);
          return next;
        });
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const cls = [
    "shell",
    sidebarMode === "collapsed" ? "shell--sidebar-collapsed" : "",
    isMac() ? "shell--titlebar-inset" : "",
  ].filter(Boolean).join(" ");

  return (
    <div className={cls}>
      <div className="shell__sidebar" data-tauri-drag-region>
        <Sidebar
          active={page}
          badge={badge}
          onSelect={onNavigate}
          collapsed={sidebarMode === "collapsed"}
          onSettings={onSettings}
          onSearch={onSearch}
        />
      </div>
      <div className="shell__content">
        {banners}
        <div className="shell__page">{children}</div>
      </div>
    </div>
  );
}
