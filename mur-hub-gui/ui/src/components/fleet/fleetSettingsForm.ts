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
  deadline: string
): boolean {
  if (trigKind === "interval" && !DURATION_RE.test(trigValue.trim())) return false;
  if (trigKind === "cron" && trigValue.trim() === "") return false;
  if (deadline.trim() !== "" && !DURATION_RE.test(deadline.trim())) return false;
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

/** Which completion policy the Settings select is showing. */
export type DonePolicyKind = "router" | "queue-empty" | "marker";

/** The `done_when` sentinel selecting the queue-drained policy. Must stay in
 *  step with mur-core's `DONE_WHEN_QUEUE_EMPTY`. */
export const DONE_WHEN_QUEUE_EMPTY = "queue-empty";

const MARKER_PREFIX = "marker:";

/**
 * Classify a stored `done_when`, mirroring mur-core's `done_policy`: anything
 * that is neither the queue sentinel nor a usable `marker:` value means "ask
 * the router" -- including legacy free-text criteria, which is exactly what the
 * backend does with them.
 */
export function parseDonePolicy(doneWhen: string): DonePolicyKind {
  const v = doneWhen.trim();
  if (v.startsWith(MARKER_PREFIX) && v.slice(MARKER_PREFIX.length).trim() !== "") return "marker";
  if (v === DONE_WHEN_QUEUE_EMPTY) return "queue-empty";
  return "router";
}

/**
 * The value to save for a chosen policy. `marker` returns the loaded expression
 * verbatim: the Hub never authors a marker, because it cannot supply the other
 * half of the contract -- something has to teach an agent to emit that text,
 * and that lives in the goal or a skill, not in this form.
 *
 * `router` returns "" rather than null on purpose. The backend treats a null as
 * "leave this field alone", so an explicit empty string is the only way to
 * clear a previously-set criterion.
 */
export function buildDoneWhen(kind: DonePolicyKind, loaded: string): string {
  if (kind === DONE_WHEN_QUEUE_EMPTY) return DONE_WHEN_QUEUE_EMPTY;
  if (kind === "marker") return loaded.trim();
  return "";
}

/** Which hint line explains the currently-selected policy. */
export const DONE_POLICY_HINT: Record<DonePolicyKind, TranslationKey> = {
  router: "fleet.settings.donePolicyHintRouter",
  "queue-empty": "fleet.settings.donePolicyHintQueueEmpty",
  marker: "fleet.settings.donePolicyHintMarker",
};
