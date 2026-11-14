//! Mirror of `mur-hub-gui/src-tauri/src/ssh_admin/view.rs`. Shapes only: the
//! SSH rules ran in mur-core before any of this reached the WebView.

import { invoke } from "@tauri-apps/api/core";

/** `ssh_admin::SSH_REQUEST_EVENT`; payload is the agent name. */
export const SSH_REQUEST_EVENT = "ssh-request-written";

export interface KeyView {
  fingerprint: string;
  comment: string;
  loaded: boolean;
  add_cmd: string | null;
}
export interface HostKeyView {
  fingerprint: string;
  key_type: string;
  matched: boolean;
}
export type VerdictName = "trusted" | "unknown" | "unknown_downgraded" | "differs";
export type SpawnName = "add_ssh" | "none_to_strict";
export interface PlanView {
  plan_id: string;
  agent: string;
  host: string;
  user: string | null;
  reason: string | null;
  keys: KeyView[];
  scanned: HostKeyView[];
  verdict: VerdictName;
  ticks_offered: boolean;
  spawn: SpawnName | null;
  consent_text: string | null;
  differs_cmd: string | null;
}
export interface Choices {
  key_fp: string | null;
  pasted: string | null;
  ticked: string[];
  spawn_consented: boolean;
  user: string;
}
export type NotReady =
  | { kind: "no_key" }
  | { kind: "key_not_loaded" }
  | { kind: "spawn_consent" }
  | { kind: "host"; detail: string }
  | { kind: "user"; detail: string };
export interface ReadyView {
  key_fp: string;
  host_keys: string[];
  spawn: SpawnName | null;
}
export interface CheckView {
  ready: ReadyView | null;
  not_ready: NotReady | null;
}
export interface ApplyView {
  lines: string[];
  restarted: boolean;
  can_restart: boolean;
  hand_back: string | null;
}
export type PairStateName = "signed" | "approved" | "not_approved";
export interface HostRuleView {
  host_key: string;
  label: string | null;
  names: string[];
  state: PairStateName;
  signed_at: string | null;
  last_expired_mark: string | null;
}
export interface KeyRuleView {
  fingerprint: string;
  label: string | null;
  loaded: boolean;
  hosts: HostRuleView[];
}
export interface PendingView {
  agent: string;
  host: string;
  port: number;
  canonical: string;
  user: string | null;
  reason: string;
  requested_at: number;
  granted: boolean;
}
export interface OverviewView {
  keys: KeyRuleView[];
  pending: PendingView[];
  decisions_unverified: string | null;
}

export const sshApi = {
  overview: (name: string) => invoke<OverviewView>("agent_ssh_overview", { name }),
  plan: (name: string, host: string, port: number | null, user: string | null) =>
    invoke<PlanView>("agent_ssh_plan", { name, host, port, user }),
  refreshKeys: (planId: string) => invoke<PlanView>("agent_ssh_refresh_keys", { planId }),
  check: (planId: string, choices: Choices) => invoke<CheckView>("agent_ssh_check", { planId, choices }),
  apply: (planId: string, choices: Choices) => invoke<ApplyView>("agent_ssh_apply", { planId, choices }),
  revoke: (name: string, keyFp: string, hostKeyFp: string | null) =>
    invoke<OverviewView>("agent_ssh_revoke", { name, keyFp, hostKeyFp }),
  dismiss: (name: string, host: string, port: number) => invoke<void>("agent_ssh_dismiss", { name, host, port }),
  pendingAll: () => invoke<PendingView[]>("ssh_pending_all"),
};
