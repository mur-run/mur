import { describe, it, expect } from "vitest";
import { expandBatch, alwaysRuleFor, grantHintFor, leafToolName, groupBatches } from "./hitlModel";
import type { PermissionsView } from "../types";

const base = { agent: "qa", prompt: "", timeout_ms: 1000, hitl_id: "h0", tool_name: "bash", tool_input: {} };

describe("expandBatch", () => {
  it("legacy payload is one call", () => {
    expect(expandBatch(base).map((r) => r.hitl_id)).toEqual(["h0"]);
  });
  it("calls become one request each, keeping their own hitl_id and hash", () => {
    const reqs = expandBatch({
      ...base,
      batch_id: "b1",
      calls: [
        { hitl_id: "h1", tool_name: "bash", tool_input: { command: "a" }, action_hash: "x".repeat(64) },
        { hitl_id: "h2", tool_name: "write_file", tool_input: { path: "/tmp/f" }, action_hash: "y".repeat(64) },
      ],
    });
    expect(reqs.map((r) => [r.hitl_id, r.tool_name, r.batch_id])).toEqual([
      ["h1", "bash", "b1"],
      ["h2", "write_file", "b1"],
    ]);
    expect(reqs[1].action_hash).toBe("y".repeat(64));
  });
});

describe("alwaysRuleFor", () => {
  it("is the exact tool name, never a glob", () => {
    expect(alwaysRuleFor("mcp__research-gateway__search")).toEqual({
      pattern: "mcp__research-gateway__search",
      policy: "allow",
    });
    expect(alwaysRuleFor("bash")?.pattern).toBe("bash");
    expect(alwaysRuleFor("mcp__x__*")).toBeNull();
  });
  it("offers nothing for spend/dispatch tools, however they are namespaced", () => {
    expect(alwaysRuleFor("fleet_run")).toBeNull();
    expect(alwaysRuleFor("mcp__mur__parallel_jobs")).toBeNull();
    expect(alwaysRuleFor("delegate_to")).toBeNull();
  });
});

describe("grantHintFor", () => {
  const perms = {
    filesystem: {
      read: [{ raw: "~/docs", expanded: "/Users/me/docs", status: "installed" }],
      write: [{ raw: "~/out", expanded: "/Users/me/out/", status: "installed" }],
      deny: [],
    },
  } as unknown as PermissionsView;
  it("is null when the path is inside a grant (write grants also cover reads)", () => {
    expect(grantHintFor("read_file", { path: "/Users/me/docs/a.md" }, perms)).toBeNull();
    expect(grantHintFor("read_file", { path: "/Users/me/out/b" }, perms)).toBeNull();
    expect(grantHintFor("write_file", { path: "/Users/me/out/b" }, perms)).toBeNull();
  });
  it("offers the parent folder with the verb the tool needs", () => {
    expect(grantHintFor("write_file", { path: "/Users/me/docs/a.md" }, perms)).toEqual({
      verb: "write",
      path: "/Users/me/docs",
    });
    expect(grantHintFor("read_file", { path: "/tmp/x/y.txt" }, perms)).toEqual({ verb: "read", path: "/tmp/x" });
  });
  it("is null for tools without a path, and without permissions to compare against", () => {
    expect(grantHintFor("bash", { command: "ls" }, perms)).toBeNull();
    expect(grantHintFor("read_file", { path: "/tmp/x" }, null)).toBeNull();
  });
});

describe("leafToolName", () => {
  it("strips the mcp prefix only", () => {
    expect(leafToolName("mcp__research-gateway__search")).toBe("search");
    expect(leafToolName("bash")).toBe("bash");
  });
});

describe("groupBatches", () => {
  it("keeps consecutive same-batch requests together and singles apart", () => {
    const r = (id: string, batch_id?: string) => ({ ...base, hitl_id: id, batch_id });
    expect(groupBatches([r("a", "b1"), r("b", "b1"), r("c"), r("d", "b2")]).map((g) => g.map((x) => x.hitl_id))).toEqual(
      [["a", "b"], ["c"], ["d"]],
    );
  });
});
