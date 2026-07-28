// TypeScript types matching mur-gui-core Rust structs.

export type AgentStatus = "running" | "idle" | "stale";

export interface AgentEntry {
  name: string;
  display_name: string;
  category: "research" | "automation" | "monitor" | "notify" | "commerce" | "custom";
  status: AgentStatus;
  model_id: string;
  /** Active pet style preset id (e.g. "chiikawa"); drives the card avatar. */
  style_preset: string;
  /** Coarse role label (e.g. "Engineer") for grouping/filtering; null if unset. */
  role: string | null;
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

// ── Appearance types ──────────────────────────────────────────────────────

export type BehaviorPreset = "quiet" | "normal" | "lively";

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
  /** Whether the backing file still parses + validates as a skill manifest. */
  loadable: boolean;
  /**
   * Why: "missing" = the file was never installed (dangling profile.yaml ref),
   * "malformed" = the file exists but no longer parses (#717).
   */
  status: "ok" | "missing" | "malformed";
}

export interface InstalledAddonView {
  id: string;
  source: string;
  enabled: boolean;
  skills: string[];
  mcp: string[];
  commands: string[];
}

export interface InstalledSkillView {
  name: string;
  version: string;
  description: string;
  category: string;
  enabled: boolean;
  addon_id?: string | null;
}

/** Returned by the `agent_skill_install` command. */
export interface SkillInstallResult {
  detail: AgentDetail;
  /** Canonical id the skill was registered as, e.g. `skills/foo.yaml`. */
  installed_id: string;
}

export interface McpServerView {
  name: string;
  command: string;
  args: string[];
  enabled: boolean;
  addon_id?: string | null;
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
  model_ref: string | null;
  model_provider: string;
  model_name: string;
  role: string | null;
  display_name: string;
  agent_name: string;
  addons: InstalledAddonView[];
}

export type { ModelOption } from "./components/modelPicker";

export interface DetailPatch {
  role?: string;
  persona_category?: string;
  persona_description?: string;
  persona_tone?: string;
  persona_risk?: string;
  persona_verbosity?: string;
  style_preset?: string;
  source_image_path?: string;
  behavior_preset?: string;
  model_ref?: string;
}

export type DetailTab =
  | "persona"
  | "style"
  | "behavior"
  | "skills"
  | "mcp"
  | "permissions"
  | "inbox"
  | "mobile"
  | "memory"
  | "plugins";

export const ALL_DETAIL_TABS: DetailTab[] = [
  "persona",
  "style",
  "behavior",
  "skills",
  "mcp",
  "permissions",
  "inbox",
  "mobile",
  "memory",
  "plugins",
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
  persona: "Persona",
  style: "Style",
  behavior: "Behavior",
  skills: "Skills",
  mcp: "MCP",
  permissions: "Permissions",
  inbox: "Inbox",
  mobile: "Mobile",
  memory: "Memory",
  plugins: "Plugins",
};

export interface MemoryView {
  relationship: string;
  formality: string;
  first_memory: string;
  sys_prompt: string;
  companion_initialised: boolean;
}

export interface MemoryPatch {
  relationship?: string;
  formality?: string;
  first_memory?: string;
  sys_prompt?: string;
}

export interface HitlRequest {
  agent: string;
  hitl_id: string;
  tool_name: string;
  tool_input: Record<string, unknown>;
  prompt: string;
  timeout_ms: number;
}

/** `nudge_status` — state of the "connect a smarter brain" nudge. */
export interface NudgeStatus {
  /** The user pressed "no thanks" at some point — never nag again. */
  dismissed: boolean;
  /** Human-readable name of the concierge's current model, for display. */
  model: string | null;
  /** Only true while the concierge is still on the brain it shipped with. */
  stock_brain: boolean;
}
