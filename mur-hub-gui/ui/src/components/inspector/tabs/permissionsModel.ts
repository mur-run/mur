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

/** What the CLI's set-mode accepts, spelled as the view/serde names. The
 *  outbound value goes to `cmd_perm_set_mode` verbatim; `proxy_only` is the
 *  CLI spelling (the DTO shows `proxyonly`, serde's lowercase of the variant). */
export const OUTBOUND_MODES = ["restricted", "unrestricted", "proxy_only", "off"] as const;
export const SPAWN_MODES = ["allowlist", "any", "none", "strict"] as const;
export const TOOL_POLICIES = ["allow", "ask", "deny"] as const;
export type OutboundMode = (typeof OUTBOUND_MODES)[number];

/** Spec §P2: restart is said, not implied. Only a running agent needs it. */
export function afterWriteHint(isRunning: boolean): "perm.restartHint" | "perm.saved" {
  return isRunning ? "perm.restartHint" : "perm.saved";
}

/** The DTO's outbound spelling → the CLI's, for the select's current value. */
export function outboundModeForCli(dto: string): OutboundMode {
  return dto === "proxyonly" ? "proxy_only" : (dto as OutboundMode);
}
