/** Pure helpers for FleetDetail's Settings section. */

import type { FleetLoopView } from "./types";

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
