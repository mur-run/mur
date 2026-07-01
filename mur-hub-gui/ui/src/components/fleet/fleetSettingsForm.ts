/** Pure helpers for FleetDetail's Settings section. */

import type { FleetLoopView } from "./types";
import { DURATION_RE } from "./fleetCreateForm";

export type TriggerKind = "manual" | "interval" | "cron";

export function parseTrigger(loopCfg: FleetLoopView | null): { kind: TriggerKind; value: string } {
  const trigger = loopCfg?.trigger ?? "manual";
  if (trigger.startsWith("interval:")) return { kind: "interval", value: trigger.slice("interval:".length) };
  if (trigger.startsWith("cron:")) return { kind: "cron", value: trigger.slice("cron:".length) };
  return { kind: "manual", value: "" };
}

export function buildTrigger(kind: TriggerKind, value: string): string {
  if (kind === "manual") return "manual";
  return `${kind}:${value.trim()}`;
}

/**
 * Gates the Settings Save button. Mirrors mur-core's parse_duration acceptance:
 * an interval trigger's value, and any non-empty deadline, must match DURATION_RE
 * (digits + optional single-char s/m/h/d suffix, e.g. 30s/5m/2h/1d -- NOT a calendar
 * date) or Save stays disabled. Cron just needs a non-empty value. An unparseable
 * value slipping through here would silently mean "no deadline enforced" / "never
 * fires" on the backend (fail-open) -- this is the safety property Task 6 review
 * flagged as needing test coverage.
 */
export function settingsAreValid(trigKind: TriggerKind, trigValue: string, deadline: string): boolean {
  if (trigKind === "interval" && !DURATION_RE.test(trigValue.trim())) return false;
  if (trigKind === "cron" && trigValue.trim() === "") return false;
  if (deadline.trim() !== "" && !DURATION_RE.test(deadline.trim())) return false;
  return true;
}
