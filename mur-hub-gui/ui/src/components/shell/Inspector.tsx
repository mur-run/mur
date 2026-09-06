//! Inspector — thin switch that renders the right contextual right-pane for the
//! current page's selection. Rendered into the Shell's inspector slot by
//! DashboardApp. When there is no active selection for the current page,
//! `hasInspector()` returns false and DashboardApp passes `undefined` so the
//! Shell auto-hides the column.

import type { ReactNode } from "react";
import type { PageId } from "./nav";
import { isLibrary } from "./nav";
import { ChatInspector } from "../inspector/ChatInspector";

export interface InspectorSelection {
  /** Agents page — selected agent name (also drives the app-wide detail). */
  agent: string | null;
  /** Chats page — active chat's agent name. */
  chatAgent: string | null;
  chatDisplayName?: string;
}

/** Whether the current page has an active selection that should show a column. */
export function hasInspector(page: PageId, sel: InspectorSelection): boolean {
  // Agents render their detail in the page (spec §3.4); no inspector column.
  if (page === "agents") return false;
  if (page === "chats") return sel.chatAgent !== null;
  if (page === "fleets") return false; // detail lives in the page (spec §3.4)
  // Library pages own their detail (Phase 2a); no inspector column.
  if (isLibrary(page)) return false;
  return false;
}

export interface InspectorProps {
  page: PageId;
  selection: InspectorSelection;
  onClose: () => void;
}

export function Inspector({ page, selection, onClose }: InspectorProps): ReactNode {
  if (page === "chats" && selection.chatAgent) {
    return (
      <ChatInspector
        agentName={selection.chatAgent}
        displayName={selection.chatDisplayName}
        onClose={onClose}
      />
    );
  }
  return null;
}
