import { describe, expect, it } from "vitest";
import { itemFor, mcpFacets, mcpRows, pluginRows, skillFacets, skillRows, workflowRows } from "./libraryModel";

const skills = [
  { name: "mur-dev", description: "dev", category: "workflow", origin_version: "1.2.0", status: "update available", agents: ["aura"], path: "/x/mur-dev" },
  { name: "mur-tdd", description: "tdd", category: "workflow", origin_version: null, status: "—", agents: [], path: null },
];
const mcps = [{ id: "fs", name: "filesystem", description: "files", transport: "stdio", agents: ["aura", "scout"] }];
const plugins = [{ id: "ghp", source: "/p/ghp", skill_count: 2, mcp_count: 1, command_count: 3, agents: [{ agent: "aura", enabled: true }] }];
const workflows = [{ name: "release", description: "cut a release", path: "/home/d/.mur/workflows/release.yaml" }];
const noAvatar = () => null;

describe("rows", () => {
  it("skills: subtitle carries category, version and status; rows have no status dot", () => {
    const r = skillRows(skills, "v", noAvatar);
    expect(r[0].subtitle).toBe("workflow · v1.2.0 · update available");
    expect(r[1].subtitle).toBe("workflow");
    expect(r[0].status).toBeUndefined();
    expect(r[0].facets).toEqual(["workflow"]);
  });
  it("mcp: subtitle is transport and usage count", () => {
    expect(mcpRows(mcps, (n) => `used by ${n}`, noAvatar)[0].subtitle).toBe("stdio · used by 2");
  });
  it("plugins and workflows", () => {
    expect(pluginRows(plugins, { skills: "skills", mcp: "MCP", commands: "commands" }, noAvatar)[0].subtitle).toBe("2 skills · 1 MCP · 3 commands");
    expect(workflowRows(workflows, noAvatar)[0].subtitle).toBe("release.yaml");
  });
});

describe("facets", () => {
  it("count per distinct value, sorted", () => {
    expect(skillFacets(skills)).toEqual([{ id: "workflow", label: "workflow", count: 2 }]);
    expect(mcpFacets(mcps)).toEqual([{ id: "stdio", label: "stdio", count: 1 }]);
  });
});

describe("itemFor", () => {
  it("maps a skill to an item with meta rows and path", () => {
    const it0 = itemFor("skill", skills[0], { category: "Category", version: "Version", status: "Status", path: "Path" });
    expect(it0.meta.map((m) => m.value)).toEqual(["workflow", "1.2.0", "update available", "/x/mur-dev"]);
    expect(it0.path).toBe("/x/mur-dev");
  });
});
