export interface FleetSummary {
  name: string;
  display_name: string;
  goal: string;
  member_count: number;
  active_jobs: number;
  stopped: boolean;
  running: boolean;
}

export interface FleetDetail {
  name: string;
  display_name: string;
  goal: string;
  router: string;
  members: string[];
  channel_id: string;
  stopped: boolean;
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
