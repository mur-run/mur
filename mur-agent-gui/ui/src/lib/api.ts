// Thin wrappers around Tauri commands. Each function corresponds to
// a `#[tauri::command]` in src-tauri/src/commands.rs.

import { invoke } from "@tauri-apps/api/core";

// ─── Status ────────────────────────────────────────────────────────
export interface StatusView {
  name: string;
  agent_home: string;
  kind: string;
  pid?: number;
  uptime_seconds?: number;
  key_version: number;
}
export const status = () => invoke<StatusView>("status");
export const startAgent = () => invoke<void>("start_agent");
export const stopAgent = () => invoke<void>("stop_agent");
export const restartAgent = () => invoke<void>("restart_agent");

// ─── System Prompt ────────────────────────────────────────────────
export const promptGet = () => invoke<string>("prompt_get");
export const promptSet = (content: string) =>
  invoke<void>("prompt_set", { content });

// ─── Skills ───────────────────────────────────────────────────────
export const skillList = () => invoke<string[]>("skill_list");
export const skillShow = (query: string) =>
  invoke<string>("skill_show", { query });
export const skillAdd = (source: string) =>
  invoke<void>("skill_add", { source });
export const skillRemove = (query: string) =>
  invoke<void>("skill_remove", { query });

// ─── MCP ──────────────────────────────────────────────────────────
export interface McpServerEntry {
  id: string;
  command: string;
  args: string[];
  env?: Record<string, string>;
}
export const mcpList = () => invoke<McpServerEntry[]>("mcp_list");
export const mcpAdd = (
  serverId: string,
  command: string,
  args: string[],
) => invoke<void>("mcp_add", { serverId, command, args });
export const mcpRemove = (serverId: string) =>
  invoke<void>("mcp_remove", { serverId });
export const mcpRename = (oldId: string, newId: string) =>
  invoke<void>("mcp_rename", { old: oldId, new: newId });

// ─── Permissions ──────────────────────────────────────────────────
// `Entitlements` mirrors mur_common::agent::Entitlements; treated as
// opaque JSON in the UI shell for now (P1.3 will type it properly).
export const permView = () => invoke<unknown>("perm_view");
export const permSetMode = (key: string, value: string) =>
  invoke<void>("perm_set_mode", { key, value });
export const permAllowHost = (glob: string) =>
  invoke<void>("perm_allow_host", { glob });
export const permDenyHost = (glob: string) =>
  invoke<void>("perm_deny_host", { glob });
export const permAllowRead = (path: string) =>
  invoke<void>("perm_allow_read", { path });
export const permAllowWrite = (path: string) =>
  invoke<void>("perm_allow_write", { path });
export const permDenyPath = (path: string) =>
  invoke<void>("perm_deny_path", { path });
export const permAllowSpawn = (binary: string) =>
  invoke<void>("perm_allow_spawn", { binary });
export const permDenySpawn = (binary: string) =>
  invoke<void>("perm_deny_spawn", { binary });
export const permSetLimit = (key: string, value: number) =>
  invoke<void>("perm_set_limit", { key, value });

// ─── Observability ────────────────────────────────────────────────
export interface StatsView {
  counters: Record<string, number>;
  files_scanned: number;
  bytes_scanned: number;
}
export const stats = () => invoke<StatsView>("stats");
export const logs = (tail: number) => invoke<string>("logs", { tail });

// ─── Theme ────────────────────────────────────────────────────────
export interface ThemeInfo {
  name: string;
  display_name: string;
  kind: string;
}
export const listThemes = () => invoke<ThemeInfo[]>("list_themes");
export const setTheme = (name: string) => invoke<void>("set_theme", { name });
