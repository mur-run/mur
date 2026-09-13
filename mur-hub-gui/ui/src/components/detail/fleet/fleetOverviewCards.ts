/** Pure helper for the fleet Overview stat row (spec §10.1): reads
 *  `FleetDetail.limits` instead of re-deriving deadline/stuck/cost from
 *  `loop_cfg` — the Hub renders what the CLI resolves. */

import type { FleetLoopView, LimitsRowView, LimitsView } from "../../fleet/types";
import type { TranslationKey } from "../../../i18n/types";

type T = (key: TranslationKey, vars?: Record<string, string | number>) => string;

const DASH = "—";

function rowValue(rows: LimitsRowView[], knob: LimitsRowView["knob"]): string {
  return rows.find((r) => r.knob === knob)?.value ?? "—";
}

/** `last_run` is an ISO timestamp from the loop record; show it in the
 *  viewer's locale when it parses, else as given. */
function lastRunLabel(raw: string | null | undefined, never: string): string {
  if (!raw) return never;
  const ms = Date.parse(raw);
  return Number.isNaN(ms) ? raw : new Date(ms).toLocaleString();
}

/** Four cards: last auto-run, the deadline in force, a cost cap when this
 *  fleet is billable and has one set (else the stuck window), and done-when. */
export function statCards(limits: LimitsView, loop: FleetLoopView | null, t: T): { value: string; label: TranslationKey }[] {
  const costRow = limits.rows.find((r) => r.knob === "cost_usd");
  const thirdCard =
    limits.billable && costRow?.local
      ? { value: costRow.value, label: "limits.card.costCap" as const }
      : { value: rowValue(limits.rows, "stuck"), label: "limits.card.stuck" as const };
  return [
    { value: lastRunLabel(loop?.last_run, t("fleet.never")), label: "fleet.settings.lastRun" },
    { value: rowValue(limits.rows, "deadline"), label: "limits.card.deadline" },
    thirdCard,
    { value: loop?.done_when || (loop ? t("fleet.settings.donePolicyRouter") : DASH), label: "fleet.settings.doneWhen" },
  ];
}
