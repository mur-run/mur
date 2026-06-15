// Types mirroring the Rust ChannelSummary / Channel / ChannelEvent DTOs.
// Kept in a separate file so tests can import without React.

export type ChannelActor =
  | { kind: "human"; name?: string }
  | { kind: "agent"; id?: string }
  | { kind: "system" };

export interface WorkParticipant {
  kind: "human" | "agent" | "system";
  id: string;
  role: string;
}

export interface ChannelSummary {
  id: string;
  title: string;
  state: string;
  goal: string;
  created_at: string;
  updated_at: string;
  participants: WorkParticipant[];
  agents: string[];
  turns: number;
  preview: string;
}

export interface ChannelEvent {
  seq: number;
  ts: string;
  actor: ChannelActor;
  kind: string;
  payload: Record<string, unknown>;
  idempotency_key?: string;
}

export interface Participant {
  actor: ChannelActor;
  role: string;
  joined_at: string;
}

export interface Goal {
  statement: string;
  acceptance_criteria: string[];
}

export interface Channel {
  v: number;
  id: string;
  title: string;
  goal: Goal;
  state: string;
  owner: ChannelActor;
  participants: Participant[];
  created_at: string;
  updated_at: string;
}
