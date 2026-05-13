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
