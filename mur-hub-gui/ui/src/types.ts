// TypeScript types matching mur-gui-core::discovery Rust structs.

export type AgentStatus = "running" | "idle" | "stale";

export interface AgentEntry {
  name: string;
  display_name: string;
  category: "research" | "automation" | "monitor" | "notify" | "commerce" | "custom";
  status: AgentStatus;
  model_id: string;
}
