// Schedule rows as the Rust aggregator serves them, and the three questions a
// reader asks of one: what does it mean, whose is it, and when does it next
// fire.
//
// Shared so the Panel and the agent inspector cannot answer them differently.
// `description` and `next_note` are derived in Rust
// (`mur_core::schedule_status`) — nothing here computes a schedule, it only
// phrases what arrived.

export type ScheduleItem =
  | {
      kind: "agent_cron";
      owner: string;
      expr: string;
      message: string;
      next_fires: string[];
      status: string;
      description: string;
      next_note: string | null;
    }
  | {
      kind: "agent_idle";
      owner: string;
      after_secs: number;
      cooldown_secs: number;
      message: string;
      status: string;
      description: string;
      next_note: string | null;
    }
  | {
      kind: "workflow";
      owner: string;
      expr?: string | null;
      next_fires: string[];
      status: string;
      description: string;
      next_note: string | null;
    }
  | {
      kind: "fleet";
      owner: string;
      trigger: string;
      next_fires: string[];
      status: string;
      budget_usd: number;
      autorun_env: boolean;
      description: string;
      next_note: string | null;
    };

export type ScheduleStatus = { schedules: ScheduleItem[]; warnings: string[] };

/// The raw expression, kept as a tooltip: the phrasing is for reading, the
/// expression is for editing, and a reader who wants to change it needs both.
export function scheduleExpr(s: ScheduleItem): string {
  switch (s.kind) {
    case "agent_cron":
      return s.expr;
    case "agent_idle":
      return `after ${s.after_secs}s idle`;
    case "workflow":
      return s.expr ?? "manual";
    case "fleet":
      return s.trigger;
  }
}

/// Which agent this row belongs to.
///
/// A pure mapping over `kind`, so it lives here rather than in the aggregator:
/// it is a label, not a derivation, and four constants cannot drift. Fleet and
/// workflow schedules are machine-wide — the panel is scoped to one agent, and
/// showing them unlabelled is what makes a five-row table look like five of
/// this agent's schedules.
export function scheduleScope(s: ScheduleItem): "agent" | "global" {
  return s.kind === "agent_cron" || s.kind === "agent_idle" ? "agent" : "global";
}

/// Whether the row runs on a clock, or only when something starts it.
///
/// `status` answers neither question: a manual fleet is "enabled" and still
/// never fires on its own, which is how a table of five rows came to show four
/// unscheduled ones as if they were scheduled. Four constants over `kind`, same
/// reasoning as `scheduleScope` — a label, not a derivation.
///
/// A cron the backend could not parse still counts as timetabled: it has a
/// timetable, and the reason it has no fire time is what `next_note` is for.
/// Calling it "manual" would replace one wrong answer with another.
export function scheduleTimetabled(s: ScheduleItem): boolean {
  switch (s.kind) {
    case "agent_cron":
      return true;
    case "agent_idle":
      return false;
    case "workflow":
      return s.expr != null;
    case "fleet":
      return s.trigger !== "manual";
  }
}

/// The rows the Panel shows, split the way it shows them.
///
/// The agent's own rows and machine-wide ones are always visible; other agents'
/// appear only when asked for. `hidden` is what the toggle is suppressing right
/// now — reported even when it is 0, because a checkbox whose effect is
/// invisible is one a reader concludes is broken.
export function panelSchedules(
  rows: ScheduleItem[],
  agent: string | null,
  showAll: boolean,
): { timed: ScheduleItem[]; manual: ScheduleItem[]; hidden: number } {
  const visible = showAll
    ? rows
    : rows.filter((s) => scheduleScope(s) === "global" || s.owner === agent);
  return {
    timed: visible.filter(scheduleTimetabled),
    manual: visible.filter((s) => !scheduleTimetabled(s)),
    hidden: rows.length - visible.length,
  };
}

export function scheduleNext(s: ScheduleItem): { text: string; title: string; muted: boolean } {
  const fires = "next_fires" in s ? s.next_fires : [];
  if (!fires.length) {
    // Never a bare dash. A blank here reads as "will not run again", and one of
    // the things it used to hide was a fleet running every thirty minutes. The
    // backend guarantees a note whenever there is no fire time.
    return { text: s.next_note ?? "not scheduled", title: s.next_note ?? "", muted: true };
  }
  const first = new Date(fires[0]);
  const text = Number.isNaN(first.getTime()) ? fires[0] : first.toLocaleString();
  return { text, title: fires.join("\n"), muted: false };
}
