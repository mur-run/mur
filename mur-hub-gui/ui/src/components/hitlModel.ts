import type { HitlBatchPayload, HitlRequest, PermissionsView } from "../types";

/** One card per call. Legacy payloads (no `calls`) are one call. */
export function expandBatch(p: HitlBatchPayload): HitlRequest[] {
  const base = { agent: p.agent, prompt: p.prompt, timeout_ms: p.timeout_ms, batch_id: p.batch_id };
  if (p.calls && p.calls.length > 0) {
    return p.calls.map((c) => ({
      ...base,
      hitl_id: c.hitl_id,
      tool_name: c.tool_name,
      tool_input: c.tool_input,
      action_hash: c.action_hash,
    }));
  }
  return [
    { ...base, hitl_id: p.hitl_id, tool_name: p.tool_name, tool_input: p.tool_input, action_hash: p.action_hash },
  ];
}

/** `mcp__srv__tool` → `tool`; anything else unchanged. */
export function leafToolName(name: string): string {
  const m = name.match(/^mcp__[^_]+(?:_[^_]+)*__(.+)$/);
  return m ? m[1] : name;
}

/** Spend / dispatch tools never get an "always": the whole point of `Ask` on
 *  them is that every spend is a human decision. */
export const NO_ALWAYS_TOOLS = ["fleet_run", "parallel_jobs", "delegate_to"] as const;

/** The exact-name rule "always" would write, or null when no rule is offered.
 *  Never a glob: the pattern is the full tool name as the runtime spells it. */
export function alwaysRuleFor(toolName: string): { pattern: string; policy: "allow" } | null {
  if ((NO_ALWAYS_TOOLS as readonly string[]).includes(leafToolName(toolName))) return null;
  if (toolName.includes("*")) return null;
  return { pattern: toolName, policy: "allow" };
}

const PATH_TOOLS: Record<string, "read" | "write"> = {
  read_file: "read",
  write_file: "write",
  edit_file: "write",
};

/** When the call names a path outside the agent's grants, the P2 grant that
 *  would let it SUCCEED. Separate from "always": a rule answers "may it run",
 *  a grant answers "can it reach". */
export function grantHintFor(
  toolName: string,
  input: Record<string, unknown>,
  perms: PermissionsView | null,
): { verb: "read" | "write"; path: string } | null {
  const verb = PATH_TOOLS[leafToolName(toolName)];
  const path = typeof input.path === "string" ? input.path : null;
  if (!verb || !path || !perms) return null;
  const granted = (
    verb === "read" ? [...perms.filesystem.read, ...perms.filesystem.write] : perms.filesystem.write
  ).map((g) => g.expanded.replace(/\/+$/, ""));
  const covered = granted.some((g) => path === g || path.startsWith(g + "/"));
  return covered ? null : { verb, path: parentDir(path) };
}

/** Grants are folders; a file path is offered as its directory. */
function parentDir(p: string): string {
  const i = p.lastIndexOf("/");
  return i > 0 ? p.slice(0, i) : p;
}

/** Group consecutive requests that share a `batch_id` so one response renders
 *  under one header. Requests without a batch id are groups of one. */
export function groupBatches(reqs: HitlRequest[]): HitlRequest[][] {
  const out: HitlRequest[][] = [];
  for (const r of reqs) {
    const last = out[out.length - 1];
    if (last && r.batch_id && last[0].batch_id === r.batch_id) last.push(r);
    else out.push([r]);
  }
  return out;
}
