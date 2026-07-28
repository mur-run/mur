import { describe, it, expect } from "vitest";
import {
  specReducer,
  SPEC_FLOW_INITIAL,
  type SpecFlowState,
} from "./specFlow";

// ── helpers ───────────────────────────────────────────────────────────────────

function s(
  step: SpecFlowState["step"],
  source: SpecFlowState["source"] = null,
): SpecFlowState {
  return { step, source };
}

// ── SELECT_SOURCE ─────────────────────────────────────────────────────────────

describe("SELECT_SOURCE on 'source' step", () => {
  it("template → 'role' step", () => {
    const next = specReducer(SPEC_FLOW_INITIAL, { type: "SELECT_SOURCE", source: "template" });
    expect(next).toEqual(s("role", "template"));
  });

  it("official → 'official' step", () => {
    const next = specReducer(SPEC_FLOW_INITIAL, { type: "SELECT_SOURCE", source: "official" });
    expect(next).toEqual(s("official", "official"));
  });

  it("import stays on 'source' — the host owns the import modal", () => {
    const next = specReducer(SPEC_FLOW_INITIAL, { type: "SELECT_SOURCE", source: "import" });
    expect(next).toEqual(s("source", "import"));
  });

  it("SELECT_SOURCE on a non-source step is a no-op", () => {
    const inRole = s("role", "template");
    const next = specReducer(inRole, { type: "SELECT_SOURCE", source: "official" });
    expect(next).toEqual(inRole);
  });
});

// ── BACK ──────────────────────────────────────────────────────────────────────

describe("BACK transitions", () => {
  it("BACK on 'source' is a no-op (already at start)", () => {
    expect(specReducer(SPEC_FLOW_INITIAL, { type: "BACK" })).toEqual(SPEC_FLOW_INITIAL);
  });

  it("BACK from 'role' → 'source' with the source cleared", () => {
    expect(specReducer(s("role", "template"), { type: "BACK" })).toEqual(s("source", null));
  });

  it("BACK from 'official' → 'source' with the source cleared", () => {
    expect(specReducer(s("official", "official"), { type: "BACK" })).toEqual(s("source", null));
  });

  it("BACK from 'generating' → 'role'", () => {
    expect(specReducer(s("generating", "template"), { type: "BACK" })).toEqual(
      s("role", "template"),
    );
  });

  it("BACK from 'review' → 'generating'", () => {
    expect(specReducer(s("review", "template"), { type: "BACK" })).toEqual(
      s("generating", "template"),
    );
  });

  it("BACK from 'eval' is a no-op (terminal: agent created, draft consumed)", () => {
    const st = s("eval", "template");
    expect(specReducer(st, { type: "BACK" })).toEqual(st);
  });

  it("BACK from 'appearance' is a no-op (the agent already exists)", () => {
    const st = s("appearance", "official");
    expect(specReducer(st, { type: "BACK" })).toEqual(st);
  });
});

// ── NEXT ──────────────────────────────────────────────────────────────────────

describe("NEXT transitions", () => {
  it("NEXT from 'role' → 'generating'", () => {
    expect(specReducer(s("role", "template"), { type: "NEXT" })).toEqual(
      s("generating", "template"),
    );
  });

  it("NEXT from 'generating' → 'review'", () => {
    expect(specReducer(s("generating", "template"), { type: "NEXT" })).toEqual(
      s("review", "template"),
    );
  });

  it("NEXT from 'review' → 'eval'", () => {
    expect(specReducer(s("review", "template"), { type: "NEXT" })).toEqual(s("eval", "template"));
  });

  it("NEXT from 'eval' → 'appearance' (every new agent gets the offer)", () => {
    expect(specReducer(s("eval", "template"), { type: "NEXT" })).toEqual(
      s("appearance", "template"),
    );
  });

  it("NEXT from 'official' → 'appearance' (an installed agent can be a pet too)", () => {
    expect(specReducer(s("official", "official"), { type: "NEXT" })).toEqual(
      s("appearance", "official"),
    );
  });

  it("NEXT on 'appearance' is a no-op (terminal)", () => {
    const st = s("appearance", "template");
    expect(specReducer(st, { type: "NEXT" })).toEqual(st);
  });

  it("NEXT on 'source' is a no-op", () => {
    expect(specReducer(SPEC_FLOW_INITIAL, { type: "NEXT" })).toEqual(SPEC_FLOW_INITIAL);
  });
});

// ── RESET ─────────────────────────────────────────────────────────────────────

describe("RESET", () => {
  it("resets from any step to initial state", () => {
    expect(specReducer(s("appearance", "official"), { type: "RESET" })).toEqual(
      SPEC_FLOW_INITIAL,
    );
  });
});

// ── Full paths ────────────────────────────────────────────────────────────────

describe("Full flow paths", () => {
  it("template: source → role → generating → review → eval → appearance", () => {
    let st = SPEC_FLOW_INITIAL;
    st = specReducer(st, { type: "SELECT_SOURCE", source: "template" });
    expect(st.step).toBe("role");
    for (const expected of ["generating", "review", "eval", "appearance"]) {
      st = specReducer(st, { type: "NEXT" });
      expect(st.step).toBe(expected);
    }
  });

  it("official: source → official → appearance", () => {
    let st = SPEC_FLOW_INITIAL;
    st = specReducer(st, { type: "SELECT_SOURCE", source: "official" });
    expect(st.step).toBe("official");
    st = specReducer(st, { type: "NEXT" });
    expect(st.step).toBe("appearance");
  });
});
