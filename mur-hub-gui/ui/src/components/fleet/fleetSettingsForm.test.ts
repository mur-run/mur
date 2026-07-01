import { describe, it, expect } from "vitest";
import { parseTrigger, buildTrigger } from "./fleetSettingsForm";

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
