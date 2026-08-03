import { describe, it, expect } from "vitest";
import {
  parseTrigger,
  buildTrigger,
  settingsAreValid,
  modeBadgeLabel,
  loopDeadlineIsValid,
  parseDonePolicy,
  buildDoneWhen,
  buildCronExpr,
} from "./fleetSettingsForm";

describe("parseTrigger", () => {
  it("null loop_cfg → manual", () => {
    expect(parseTrigger(null)).toEqual({ kind: "manual", value: "" });
  });
  it("splits interval:<dur>", () => {
    expect(
      parseTrigger({ trigger: "interval:30m", max_iterations: 0, budget_usd: 0, deadline: "", done_when: "", last_run: null })
    ).toEqual({ kind: "interval", value: "30m" });
  });
  it("splits cron:<expr>", () => {
    expect(
      parseTrigger({ trigger: "cron:*/15 * * * *", max_iterations: 0, budget_usd: 0, deadline: "", done_when: "", last_run: null })
    ).toEqual({ kind: "cron", value: "*/15 * * * *" });
  });
});

describe("buildTrigger", () => {
  it("manual ignores value", () => {
    expect(buildTrigger("manual", "whatever")).toBe("manual");
  });
  it("interval/cron prepend prefix and trim", () => {
    expect(buildTrigger("interval", " 30m ")).toBe("interval:30m");
    expect(buildTrigger("cron", "*/15 * * * *")).toBe("cron:*/15 * * * *");
  });
});

describe("settingsAreValid", () => {
  // Regression scenario for the Task 6 review finding: a calendar-date-shaped
  // deadline must NOT slip through, because parse_duration on the backend only
  // accepts digits + optional single-char s/m/h/d suffix -- an unparseable value
  // silently means "no deadline enforced" (fail-open).
  it("rejects a calendar-date-shaped deadline (manual trigger)", () => {
    expect(settingsAreValid("manual", "", "2026-12-31")).toBe(false);
  });

  it("accepts a valid duration deadline", () => {
    expect(settingsAreValid("manual", "", "2h")).toBe(true);
  });

  it("rejects an interval trigger with a non-duration value", () => {
    expect(settingsAreValid("interval", "2026-12-31", "")).toBe(false);
  });

  it("accepts an interval trigger with a valid duration value", () => {
    expect(settingsAreValid("interval", "30m", "")).toBe(true);
  });

  it("accepts manual trigger with empty deadline (nothing configured)", () => {
    expect(settingsAreValid("manual", "", "")).toBe(true);
  });
});

describe("loopDeadlineIsValid", () => {
  // Regression scenario mirroring settingsAreValid's: the Run-as-loop panel's
  // deadline override is sent straight to fleet_run_loop, so a calendar-date-shaped
  // value must NOT slip through -- parse_duration on the backend only accepts
  // digits + optional single-char s/m/h/d suffix, and silently drops anything else
  // (fail-open: "no deadline enforced").
  it("rejects a calendar-date-shaped deadline", () => {
    expect(loopDeadlineIsValid("2026-12-31")).toBe(false);
  });

  it("accepts a valid duration", () => {
    expect(loopDeadlineIsValid("2h")).toBe(true);
  });

  it("accepts an empty string (no override)", () => {
    expect(loopDeadlineIsValid("")).toBe(true);
  });
});

describe("modeBadgeLabel", () => {
  const t = (key: string) =>
    ({
      "fleet.create.mode.speculative": "Speculative",
      "fleet.create.mode.partition": "Partition",
      "fleet.run.tracksSuffix": "tracks",
    })[key] ?? key;

  it("null summary → null", () => {
    expect(modeBadgeLabel(null, t)).toBeNull();
  });
  it("speculative → mode · count tracks", () => {
    expect(modeBadgeLabel({ mode: "speculative", track_count: 2, target_file: null }, t)).toBe(
      "Speculative · 2 tracks"
    );
  });
  it("partition → mode · target_file", () => {
    expect(modeBadgeLabel({ mode: "partition", track_count: 0, target_file: "src/widget.rs" }, t)).toBe(
      "Partition · src/widget.rs"
    );
  });
});

describe("parseDonePolicy", () => {
  it("maps a stored done_when to a policy, treating legacy criteria as router", () => {
    expect(parseDonePolicy("marker:RESEARCH_COMPLETE")).toBe("marker");
    expect(parseDonePolicy("queue-empty")).toBe("queue-empty");
    expect(parseDonePolicy("")).toBe("router");
    // Free-text criteria predate this vocabulary and mean "ask the router",
    // which is what the backend already does with them.
    expect(parseDonePolicy("all_tasks_done")).toBe("router");
    // A prefix with nothing after it is not a usable marker.
    expect(parseDonePolicy("marker:")).toBe("router");
  });
});

describe("buildDoneWhen", () => {
  it("writes an empty string for router, which is how the field gets cleared", () => {
    // `doneWhen.trim() || null` used to send null here, and the backend reads
    // null as "leave alone" -- so the Hub could not clear done_when at all.
    expect(buildDoneWhen("router", "marker:DONE")).toBe("");
    expect(buildDoneWhen("queue-empty", "")).toBe("queue-empty");
    // The Hub never authors a marker; it only preserves the loaded one.
    expect(buildDoneWhen("marker", "marker:RESEARCH_COMPLETE")).toBe("marker:RESEARCH_COMPLETE");
  });
});

describe("buildCronExpr", () => {
  it("composes a cron expression from a shape and a HH:MM time", () => {
    // Hourly uses the minute only -- the hour a user picked is meaningless for
    // "every hour", and silently keeping it would make 09:15 fire once a day.
    expect(buildCronExpr("hourly", "09:15")).toBe("15 * * * *");
    expect(buildCronExpr("daily", "09:05")).toBe("5 9 * * *");
    expect(buildCronExpr("weekdays", "18:00")).toBe("0 18 * * 1-5");
    // A native time input is empty until touched; midnight is the safe read.
    expect(buildCronExpr("daily", "")).toBe("0 0 * * *");
  });
});
