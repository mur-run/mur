//! Mirror of `mur-hub-gui/src-tauri/src/browser_admin/view.rs`. Shapes only:
//! every browser rule ran in mur-core before this reached the WebView.

import { invoke } from "@tauri-apps/api/core";

/** `browser_admin::BROWSER_REQUEST_EVENT`; payload is the agent name. */
export const BROWSER_REQUEST_EVENT = "browser-request-written";

export interface PendingView {
  agent: string;
  site: string;
  login: boolean;
  reason: string;
  requested_at: string;
}

export type OfferView =
  | { kind: "already_bound" }
  | { kind: "offered" }
  | { kind: "adds_hosts"; missing: string[] }
  | { kind: "replace_only"; with: string; drops: string[]; adds: string[] }
  | { kind: "disabled"; why: string };

export interface RowView {
  name: string;
  domains: string[];
  shared_with: string[];
  offer: OfferView;
}

export type ReachView =
  | { kind: "allowed" }
  | { kind: "needs_host"; missing: string[] }
  | { kind: "unscoped"; mode: string }
  | { kind: "disabled"; why: string };

export type BodyView =
  | { kind: "no_live_entry" }
  | { kind: "login"; rows: RowView[]; login_again: string; login_again_exists: boolean }
  | { kind: "reach"; reach: ReachView };

export interface CardView {
  id: string;
  agent: string;
  site: string;
  login: boolean;
  reason: string;
  body: BodyView;
  /** D1: the port step is CLI-only; this is the line to show for it. */
  port_line: string | null;
}

export type PickArg =
  | { kind: "row"; name: string; replace: boolean }
  | { kind: "login"; reauth: boolean; replace: boolean }
  | { kind: "allow" }
  | { kind: "dismiss" };

export interface AnsweredView {
  state: "done" | "port_failed" | "refused" | "stale";
  lines: string[];
  saved_as: string | null;
  status: "saved_not_tried" | "agent_reported" | null;
  reported_at: string | null;
  hand_back: string | null;
  kept: boolean;
}

export const browserApi = {
  pending: (name: string) => invoke<PendingView[]>("agent_browser_pending", { name }),
  card: (name: string, site: string) => invoke<CardView>("agent_browser_card", { name, site }),
  answer: (id: string, pick: PickArg) => invoke<AnsweredView>("agent_browser_answer", { id, pick }),
  loggedIn: (id: string) => invoke<void>("agent_browser_logged_in", { id }),
  close: (id: string) => invoke<void>("agent_browser_close", { id }),
};
