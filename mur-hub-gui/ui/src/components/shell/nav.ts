// Sidebar navigation model — Hub redesign Phase 1 three-pane shell.
// Order and grouping per spec §1: workspace items first, then library items.

import type { TranslationKey } from "../../i18n/types";

export type PageId =
  | "home"
  | "chats"
  | "agents"
  | "fleets"
  | "skills"
  | "workflows"
  | "mcp"
  | "models"
  | "plugins"
  | "settings";

export interface NavItem {
  id: PageId;
  labelKey: TranslationKey;
  group: "workspace" | "library";
}

export const NAV_ITEMS: NavItem[] = [
  { id: "home", labelKey: "nav.home", group: "workspace" },
  { id: "chats", labelKey: "nav.chats", group: "workspace" },
  { id: "agents", labelKey: "nav.agents", group: "workspace" },
  { id: "fleets", labelKey: "nav.fleets", group: "workspace" },
  { id: "skills", labelKey: "nav.skills", group: "library" },
  { id: "workflows", labelKey: "nav.workflows", group: "library" },
  { id: "mcp", labelKey: "nav.mcp", group: "library" },
  { id: "models", labelKey: "nav.models", group: "library" },
  { id: "plugins", labelKey: "nav.plugins", group: "library" },
];

export function isLibrary(id: PageId): boolean {
  return NAV_ITEMS.find((i) => i.id === id)?.group === "library";
}

/** Type guard for page ids arriving over events (`open-page`). Settings is a
 *  page without a nav item (spec 3(d) §3), so it is listed here explicitly. */
export function isPageId(id: string): id is PageId {
  return id === "settings" || NAV_ITEMS.some((n) => n.id === id);
}
