export interface FleetSummary {
  name: string;
  display_name: string;
  goal: string;
  member_count: number;
  active_jobs: number;
  stopped: boolean;
  running: boolean;
  /** Label ids, primary first. Empty means ungrouped. */
  labels: string[];
}

export interface LabelView {
  id: string;
  display: string;
  color: string | null;
  fleet_count: number;
}

export interface FleetLoopView {
  trigger: string;
  max_iterations: number;
  budget_usd: number;
  deadline: string;
  done_when: string;
  last_run: string | null;
}

export interface ParallelSummary {
  mode: "speculative" | "partition";
  track_count: number;
  target_file: string | null;
}

export interface FleetDetail {
  name: string;
  display_name: string;
  goal: string;
  router: string;
  members: string[];
  channel_id: string;
  stopped: boolean;
  loop_cfg: FleetLoopView | null;
  parallel_summary: ParallelSummary | null;
}

export interface JobRow {
  id: string;
  text: string;
  status: "queued" | "running" | "done" | "failed" | "canceled";
  created_at: string;
  finished_at?: string;
  result?: string;
  error?: string;
  source?: string;
  started_at?: string;
  run_id?: string;
}
