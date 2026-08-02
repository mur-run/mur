/** Pure helpers for FleetDetail's Settings section. */

import type { FleetLoopView, ParallelSummary } from "./types";
import { DURATION_RE } from "./fleetCreateForm";
import type { TranslationKey } from "../../i18n/types";

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
export function settingsAreValid(
  trigKind: TriggerKind,
  trigValue: string,
  deadline: string,
  doneWhen = ""
): boolean {
  if (trigKind === "interval" && !DURATION_RE.test(trigValue.trim())) return false;
  if (trigKind === "cron" && trigValue.trim() === "") return false;
  if (deadline.trim() !== "" && !DURATION_RE.test(deadline.trim())) return false;
  // Same fail-open shape: the backend only recognises a `marker:` prefix
  // (strip_prefix in loop_run.rs). Anything else silently falls back to router
  // convergence, so a typo'd "DONE" would look configured but do nothing.
  if (doneWhen.trim() !== "" && !doneWhen.trim().startsWith("marker:")) return false;
  return true;
}

/**
 * Gates the Run-as-loop panel's Go button for its deadline override field.
 * Same fail-open risk as settingsAreValid's deadline check: empty means "no
 * override" (valid); non-empty must match DURATION_RE, or the backend
 * silently treats an unparseable value as "no deadline enforced".
 */
export function loopDeadlineIsValid(deadline: string): boolean {
  return deadline.trim() === "" || DURATION_RE.test(deadline.trim());
}

/**
 * Formats the fleet detail header's Mode badge for Speculative/Partition
 * fleets. Returns null for Plain fleets (no `parallel_summary`), in which
 * case the caller renders no badge at all.
 */
export function modeBadgeLabel(
  summary: ParallelSummary | null,
  t: (key: TranslationKey, vars?: Record<string, string | number>) => string
): string | null {
  if (!summary) return null;
  if (summary.mode === "speculative") {
    return `${t("fleet.create.mode.speculative")} · ${summary.track_count} ${t("fleet.run.tracksSuffix")}`;
  }
  return `${t("fleet.create.mode.partition")} · ${summary.target_file ?? ""}`;
}
