import { describe, it, expect } from "vitest";
import { parseTrigger, buildTrigger, settingsAreValid, modeBadgeLabel } from "./fleetSettingsForm";

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
