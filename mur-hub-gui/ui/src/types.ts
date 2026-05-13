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
