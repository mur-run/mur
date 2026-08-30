import { describe, expect, it } from "vitest";

import { scheduleNext, scheduleScope, type ScheduleItem } from "./schedule";

function fleet(over: Partial<Extract<ScheduleItem, { kind: "fleet" }>> = {}) {
  return {
    kind: "fleet" as const,
    owner: "smoke",
    trigger: "interval:30m",
    next_fires: [],
    status: "enabled",
    budget_usd: 5,
    autorun_env: false,
    description: "every 30m",
    next_note: "not tracked — an interval fires relative to its last run",
    ...over,
  };
}

describe("scheduleNext", () => {
  // The bug this module exists for: a fleet running every thirty minutes
  // rendered as "—", which reads as "will not run again".
  it("never renders a bare dash when there is no fire time", () => {
    const out = scheduleNext(fleet());
    expect(out.text).not.toBe("—");
    expect(out.text).toContain("last run");
    expect(out.muted).toBe(true);
  });

  it("falls back to a phrase rather than a blank if the note is missing", () => {
    // The backend guarantees a note, but a client that renders "" when the
    // guarantee is broken reintroduces exactly the blank being removed.
    const out = scheduleNext(fleet({ next_note: null }));
    expect(out.text.trim().length).toBeGreaterThan(0);
    expect(out.text).not.toBe("—");
  });

  it("shows the first fire time and keeps the rest as a tooltip", () => {
    const out = scheduleNext(
      fleet({
        next_fires: ["2026-08-30T19:30:00Z", "2026-08-30T19:45:00Z"],
        next_note: null,
      }),
    );
    expect(out.muted).toBe(false);
    expect(out.title).toContain("19:45");
  });
});

describe("scheduleScope", () => {
  // Fleet and workflow schedules are machine-wide. Shown unlabelled next to an
  // agent's own, they make other people's schedules look like this agent's.
  it("separates the agent's own rows from machine-wide ones", () => {
    expect(scheduleScope(fleet())).toBe("global");
    expect(
      scheduleScope({
        kind: "workflow",
        owner: "nightly",
        expr: "0 3 * * *",
        next_fires: [],
        status: "enabled",
        description: "daily at 03:00",
        next_note: null,
      }),
    ).toBe("global");
    expect(
      scheduleScope({
        kind: "agent_cron",
        owner: "mur",
        expr: "0 9 * * *",
        message: "hi",
        next_fires: [],
        status: "enabled",
        description: "daily at 09:00",
        next_note: null,
      }),
    ).toBe("agent");
    expect(
      scheduleScope({
        kind: "agent_idle",
        owner: "mur",
        after_secs: 3600,
        cooldown_secs: 60,
        message: "yo",
        status: "enabled",
        description: "after 3600s idle",
        next_note: "no fixed time",
      }),
    ).toBe("agent");
  });
});
