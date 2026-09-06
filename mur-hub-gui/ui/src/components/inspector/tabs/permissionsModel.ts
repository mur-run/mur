import type { Enforcement } from "../../../types";

export type Tone = "ok" | "attention" | "muted";

/** Spec 2026-09-07 §1.3.1: the banner's tone. Advisory is the loud one —
 *  the agent can reach MORE than the list, the opposite of every other state. */
export function enforcementTone(e: Enforcement): Tone {
  if (e === "enforcing") return "ok";
  if (e === "advisory") return "attention";
  return "muted";
}

/** Spec §1.3.4: each block carries the CLI command that changes it. P1 has
 *  no editing, so a user who can see a grant is told how to change it. */
export function permCommands(agent: string) {
  return {
    hosts: `mur agent perm allow-host ${agent} <host>`,
    paths: `mur agent perm allow-write ${agent} <path>`,
    spawn: `mur agent perm allow-spawn ${agent} <program>`,
    tools: `mur agent perm set-tool ${agent} <tool> allow|ask|deny`,
    mcp: `mur agent mcp set-network ${agent} <server> --allow-host <host>`,
  };
}
