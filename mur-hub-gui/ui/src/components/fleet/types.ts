export interface FleetSummary {
  name: string;
  display_name: string;
  goal: string;
  member_count: number;
  active_jobs: number;
  stopped: boolean;
  running: boolean;
}

export interface FleetLoopView {
  trigger: string;
  max_iterations: number;
  budget_usd: number;
  deadline: string;
  done_when: string;
  last_run: string | null;
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
