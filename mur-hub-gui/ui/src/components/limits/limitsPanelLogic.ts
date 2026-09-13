/** Pure helpers for LimitsPanel (spec §10.1). No default or applicability
 *  rule is decided here — every field comes from `limits_resolve`; this
 *  module only decides which affordance a row gets. */

import type { LimitsRowView, LimitsView } from "../fleet/types";
import type { TranslationKey } from "../../i18n/types";

export type RowAction = "override" | "reset" | "none";

/** A non-applying row (cost_usd on a scope that cannot spend) has no
 *  action — a disabled input would read as "you are not allowed"; the
 *  truth is "this does not exist for you". A local value gets Reset
 *  (deletes the key); an inherited one gets Override. */
export function rowAction(row: LimitsRowView): RowAction {
  if (!row.applies) return "none";
  return row.local ? "reset" : "override";
}

/** Inherited (not set at this scope) renders dimmed. */
export function rowIsDimmed(row: LimitsRowView): boolean {
  return row.applies && !row.local;
}

/** Names the scope a value's source implies, in the user's words. */
export function chipLabel(
  row: LimitsRowView,
  scope: LimitsView["scope"],
  t: (key: TranslationKey) => string
): string {
  const source = row.source;
  if (source.includes("legacy")) return t("limits.chip.legacy");
  if (source === "built-in default") return t("limits.chip.builtIn");
  if (source === "~/.mur/config.yaml") return t("limits.chip.config");
  if (source === "fleet.yaml") return t(scope === "fleet" ? "limits.chip.thisFleet" : "limits.chip.fleet");
  if (source === "profile.yaml") return t(scope === "agent" ? "limits.chip.thisAgent" : "limits.chip.agent");
  if (source === "command-line flag") return t("limits.chip.flag");
  return source;
}

/** A patch for `limits_set`: exactly one knob, the string the user typed. */
export function patchForSave(
  knob: LimitsRowView["knob"],
  draft: string
): { deadline?: string; stuck?: string; cost_usd?: number } {
  const value = draft.trim();
  if (knob === "cost_usd") return { cost_usd: Number(value) };
  return { [knob]: value } as { deadline: string } | { stuck: string };
}

/** A patch for `limits_set` that deletes the key — never writes the
 *  parent's resolved value, so a later change upstream still flows down. */
export function patchForReset(knob: LimitsRowView["knob"]): { unset: string[] } {
  return { unset: [knob] };
}

/** §5 made visible: bounded / bounded-by-deadline-only (amber) / unbounded. */
export function badgeOf(
  v: LimitsView,
  t: (key: TranslationKey) => string
): { text: string; tone: "ok" | "amber" | "off" } {
  switch (v.bounded) {
    case "bounded":
      return { text: t("limits.badge.bounded"), tone: "ok" };
    case "deadline_only":
      return { text: t("limits.badge.deadlineOnly"), tone: "amber" };
    default:
      return { text: t("limits.badge.unbounded"), tone: "off" };
  }
}
