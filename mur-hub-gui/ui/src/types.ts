// TypeScript types matching mur-gui-core Rust structs.

export type AgentStatus = "running" | "idle" | "stale";

export interface AgentEntry {
  name: string;
  display_name: string;
  category: "research" | "automation" | "monitor" | "notify" | "commerce" | "custom";
  status: AgentStatus;
  model_id: string;
}

// RuntimeState matches mur-gui-core::sidecar::RuntimeState (#[serde(tag = "state")])
export type RuntimeState =
  | { state: "running"; pid: number }
  | { state: "stopped" }
  | { state: "restarting"; attempt: number; backoff_secs: number }
  | { state: "failed" };

export interface AgentRuntimeStatus {
  name: string;
  state: RuntimeState;
}

// ── Wizard types (M-h4) ───────────────────────────────────────────────────

export type WizardPersona =
  | "research"
  | "automation"
  | "monitor"
  | "notify"
  | "commerce"
  | "custom";

export type BehaviorPreset = "quiet" | "normal" | "lively";

export interface RenderProgressSnapshot {
  total: number;
  done: number;
  failed: number;
}

/** Full wizard state returned by every wizard_* command. */
export interface WizardSnapshot {
  step: number;
  persona: WizardPersona | null;
  name: string | null;
  description: string | null;
  style_preset_id: string | null;
  preset_family: string | null;
  behavior_preset: BehaviorPreset | null;
  needs_photo: boolean;
  source_photo: string | null;
  render_progress: RenderProgressSnapshot | null;
  render_done: boolean;
}

/** Built-in preset summary for the style picker. */
export interface PresetSummary {
  id: string;
  display_name: string;
  family: string;
  description: string;
}

export const BUILTIN_PRESETS: PresetSummary[] = [
  { id: "chiikawa",      display_name: "ちいかわ",       family: "chibi",    description: "Nagano-style soft pastel chibi" },
  { id: "sanrio-pastel", display_name: "Sanrio Pastel",  family: "chibi",    description: "Sanrio kawaii pastel style" },
  { id: "sumikko",       display_name: "Sumikko Gurashi", family: "chibi",   description: "Shy corner creatures" },
  { id: "shimeji-retro", display_name: "Shimeji Retro",  family: "pixel",    description: "Pixel / retro 8-bit sprite" },
  { id: "vtuber-soft",   display_name: "VTuber Soft",    family: "chibi",    description: "Soft anime VTuber style" },
  { id: "family-photo",  display_name: "Family Photo",   family: "polaroid", description: "Cartoon-ify your photo" },
];

// ── Detail panel types (Plan 3) ──────────────────────────────────────────────

export interface SkillView {
  path: string;
}

export interface InstalledSkillView {
  name: string;
  version: string;
  description: string;
  category: string;
}

export interface McpServerView {
  name: string;
  command: string;
  args: string[];
}

export type RenderStatusView =
  | { status: "pending" }
  | { status: "rendering"; done: number; total: number }
  | { status: "ready" }
  | { status: "failed"; reason: string };

export interface AgentDetail {
  persona_category: string;
  persona_description: string;
  persona_tone: string;
  persona_risk: string;
  persona_verbosity: string;
  style_preset: string;
  render_status: RenderStatusView;
  behavior_preset: string;
  skills: SkillView[];
  installed_skills: InstalledSkillView[];
  mcp_servers: McpServerView[];
  capabilities: string[];
  display_name: string;
  agent_name: string;
}

export interface DetailPatch {
  persona_category?: string;
  persona_description?: string;
  persona_tone?: string;
  persona_risk?: string;
  persona_verbosity?: string;
  style_preset?: string;
  behavior_preset?: string;
}

export type DetailTab =
  | "chat"
  | "persona"
  | "style"
  | "behavior"
  | "skills"
  | "mcp"
  | "permissions"
  | "inbox"
  | "mobile";

export const ALL_DETAIL_TABS: DetailTab[] = [
  "chat",
  "persona",
  "style",
  "behavior",
  "skills",
  "mcp",
  "permissions",
  "inbox",
  "mobile",
];

export interface NotifConfig {
  enabled: boolean;
  daily_cap: number;
  quiet_hours_enabled: boolean;
  quiet_start: string;
  quiet_end: string;
}

export interface NotifPatch {
  enabled?: boolean;
  daily_cap?: number;
  quiet_hours_enabled?: boolean;
  quiet_start?: string;
  quiet_end?: string;
}

export const TAB_LABELS: Record<DetailTab, string> = {
  chat: "Chat",
  persona: "Persona",
  style: "Style",
  behavior: "Behavior",
  skills: "Skills",
  mcp: "MCP",
  permissions: "Permissions",
  inbox: "Inbox",
  mobile: "Mobile",
};

export interface HitlRequest {
  agent: string;
  hitl_id: string;
  tool_name: string;
  tool_input: Record<string, unknown>;
  prompt: string;
  timeout_ms: number;
}
