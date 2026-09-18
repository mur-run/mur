import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import {
  OfficialPlanPreview,
  decideOfficialSelection,
  installOfficialItem,
  previewOfficialModels,
  reduceOfficialFlow,
} from "./SpecOfficial";

beforeEach(() => invoke.mockReset());

const requirements = { chat: true, tools: true, minimum_context_window: 16_000 };

const compatibleItem = {
  id: "agents/orchestrator",
  tier: "free",
  version: "1.0.0",
  description: "Delegates work",
  agent_name: "orchestrator",
  model_requirements: requirements,
  min_mur_version: "2.85.0",
  compatible: true,
  compatibility_error: null,
};

describe("official model selection bridge", () => {
  it("previews privacy policy with the backend-owned planner", async () => {
    const plan = { primary: "local", fallbacks: ["backup"], warnings: [] };
    invoke.mockResolvedValue(plan);

    await expect(previewOfficialModels("agents/orchestrator", "privacy-first")).resolves.toEqual(plan);
    expect(invoke).toHaveBeenCalledWith("official_plan_models", {
      id: "agents/orchestrator",
      policy: "privacy-first",
    });
  });

  it("leaves a no-candidate error on the policy stage with no installable plan", () => {
    expect(reduceOfficialFlow(
      { stage: "policy", plan: null, error: null },
      { type: "plan-failed", error: "no eligible model" },
    )).toEqual({
      stage: "policy",
      plan: null,
      error: "no eligible model",
    });
    expect(invoke).not.toHaveBeenCalled();
  });

  it("confirms the exact preview policy, not a TypeScript-sorted chain", async () => {
    invoke.mockResolvedValue({ item_id: "agents/orchestrator", agent_name: "orchestrator", model_selection: null });

    await installOfficialItem("agents/orchestrator", { Automatic: "cost-first" });
    expect(invoke).toHaveBeenCalledWith("official_install", {
      id: "agents/orchestrator",
      modelSelection: { Automatic: "cost-first" },
    });
  });

  it("keeps no-requirement items on the unchanged one-click path", async () => {
    invoke.mockResolvedValue({ item_id: "fleets/research", agent_name: null, model_selection: null });

    await installOfficialItem("fleets/research", "Unchanged");
    expect(invoke).toHaveBeenCalledWith("official_install", {
      id: "fleets/research",
      modelSelection: "Unchanged",
    });
  });
});

describe("official item decision", () => {
  it("blocks an item requiring a newer MUR before preview or install", () => {
    expect(decideOfficialSelection({
      ...compatibleItem,
      compatible: false,
      compatibility_error: "upgrade MUR before installing",
    })).toEqual({ kind: "blocked", error: "upgrade MUR before installing" });
    expect(invoke).not.toHaveBeenCalled();
  });

  it("opens policy selection for a compatible requirements item", () => {
    expect(decideOfficialSelection(compatibleItem)).toEqual({ kind: "policy" });
  });

  it("retains one-click installation for an item without requirements", () => {
    expect(decideOfficialSelection({ ...compatibleItem, model_requirements: null })).toEqual({
      kind: "install",
      selection: "Unchanged",
    });
  });
});

describe("official plan preview", () => {
  it("renders backend order and warnings without sorting in TypeScript", () => {
    const html = renderToStaticMarkup(
      <OfficialPlanPreview
        plan={{
          primary: "frontier",
          fallbacks: ["local-z", "local-a"],
          warnings: [{ UnknownPrice: { model_ref: "frontier" } }],
        }}
        labels={{ primary: "Primary", fallbacks: "Fallbacks", warning: () => "Unknown price" }}
      />,
    );
    expect(html).toContain("Primary: frontier");
    expect(html).toContain("Fallbacks: local-z → local-a");
    expect(html).toContain("Unknown price: frontier");
  });
});
