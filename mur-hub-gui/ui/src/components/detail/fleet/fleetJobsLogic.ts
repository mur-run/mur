/** Pure helpers for the Jobs tab (spec §10.2): a stop reason from the fleet
 *  channel outranks the plain job status, so a job that hit a guard says
 *  which one instead of just "failed". */

import type { JobRow } from "../../fleet/types";
import type { TranslationKey } from "../../../i18n/types";

type T = (key: TranslationKey, vars?: Record<string, string | number>) => string;

export function statusLabel(job: JobRow, t: T): string {
  if (job.stop_reason === "converged") return t("limits.finished");
  if (job.stop_reason) return t("limits.stopped", { reason: job.stop_reason });
  return t(`fleet.status.${job.status}` as TranslationKey);
}

/** Which limits knob a stop reason names, for the Adjust button's focus —
 *  `null` for reasons that name no single knob (converged, max-iterations,
 *  stopped, commander-killed, queue-drained, awaiting-approval). */
export function knobFor(reason: string): "deadline" | "stuck" | "cost_usd" | null {
  switch (reason) {
    case "deadline":
      return "deadline";
    case "stuck":
      return "stuck";
    case "budget":
      return "cost_usd";
    default:
      return null;
  }
}
