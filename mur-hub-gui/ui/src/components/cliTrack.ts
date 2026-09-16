/**
 * cliTrack.ts — which of the two tracks a vendor may offer.
 *
 * The spec's selection table, as a pure function. No React, no Tauri: the
 * table is the behaviour, so it is tested as a table.
 *
 * The rule that shapes every branch: `unknown` never disables a control and
 * never routes silently to CLI spawn. `false` is the gateway's own denial;
 * `unknown` only means we could not ask, and falling back costs a second
 * login and — for codex — an unmediated shell. Too much to spend on a
 * question we merely failed to ask, when the answer may be that the gateway
 * works.
 */

import type { HookState } from "./chatgptSubscription";

/** Whether the CLI is installed, and whether its safety probes passed. */
export type CliGate = "absent" | "passed" | "failed";

export type GatewayOffer = "preferred" | "cta" | "none";
export type CliSpawnOffer = "offered" | "available" | "disabled" | "none";

export interface TrackOffer {
  gateway: GatewayOffer;
  cliSpawn: CliSpawnOffer;
}

export function trackOffer(hook: HookState, cli: CliGate): TrackOffer {
  // The gateway works. Nothing else is needed, and the CLI track would only
  // charge a second login for the same capability.
  if (hook === "true") return { gateway: "preferred", cliSpawn: "none" };

  // Could not ask. Resolve that first — but never by removing a control.
  if (hook === "unknown") {
    if (cli === "passed") return { gateway: "cta", cliSpawn: "available" };
    if (cli === "failed") return { gateway: "cta", cliSpawn: "disabled" };
    return { gateway: "cta", cliSpawn: "none" };
  }

  // hook === "false": this build denied it, so the gateway CTA would be the
  // useless-repair loop. The CLI track is the only way through.
  if (cli === "passed") return { gateway: "none", cliSpawn: "offered" };
  if (cli === "failed") return { gateway: "none", cliSpawn: "disabled" };
  return { gateway: "none", cliSpawn: "none" };
}
