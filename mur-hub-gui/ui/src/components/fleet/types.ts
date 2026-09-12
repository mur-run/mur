/** How often the fleet list and detail re-read job counts: other processes
 *  (the CLI, an agent's `fleet_run`) queue jobs with no event to this window. */
export const JOBS_POLL_MS = 5_000;

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
  limits: LimitsView;
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

export interface LimitsRowView {
  knob: "deadline" | "stuck" | "cost_usd";
  value: string;
  source: string;
  note: string | null;
  applies: boolean;
  local: boolean;
  raw: string | null;
}

export interface StaleView {
  key: string;
  file: string;
  value: string;
  message: string;
}

export interface LimitsView {
  scope: "global" | "fleet" | "agent";
  name: string | null;
  rows: LimitsRowView[];
  stale: StaleView[];
  billable: boolean;
  needs_restart: boolean;
  attended_note: string;
  bounded: "bounded" | "deadline_only" | "unbounded";
  error: string | null;
}
