import { describe, it, expect } from "vitest";
import {
  specReducer,
  SPEC_FLOW_INITIAL,
  type SpecFlowState,
} from "./specFlow";

// ── helpers ───────────────────────────────────────────────────────────────────

function s(step: SpecFlowState["step"], kind: SpecFlowState["kind"] = null): SpecFlowState {
  return { step, kind };
}

// ── SELECT_KIND ───────────────────────────────────────────────────────────────

describe("SELECT_KIND on 'kind' step", () => {
  it("companion → 'companion' step", () => {
    const next = specReducer(SPEC_FLOW_INITIAL, { type: "SELECT_KIND", kind: "companion" });
    expect(next).toEqual(s("companion", "companion"));
  });

  it("specialist → 'role' step", () => {
    const next = specReducer(SPEC_FLOW_INITIAL, { type: "SELECT_KIND", kind: "specialist" });
    expect(next).toEqual(s("role", "specialist"));
  });

  it("both → 'role' step (both also goes through specialist flow)", () => {
    const next = specReducer(SPEC_FLOW_INITIAL, { type: "SELECT_KIND", kind: "both" });
    expect(next).toEqual(s("role", "both"));
  });

  it("SELECT_KIND on a non-kind step is a no-op", () => {
    const inRole = s("role", "specialist");
    const next = specReducer(inRole, { type: "SELECT_KIND", kind: "companion" });
    expect(next).toEqual(inRole);
  });
});

// ── BACK ──────────────────────────────────────────────────────────────────────

describe("BACK transitions", () => {
  it("BACK on 'kind' is a no-op (already at start)", () => {
    expect(specReducer(SPEC_FLOW_INITIAL, { type: "BACK" })).toEqual(SPEC_FLOW_INITIAL);
  });

  it("BACK from 'companion' → 'kind' with kind cleared", () => {
    const next = specReducer(s("companion", "companion"), { type: "BACK" });
    expect(next).toEqual(s("kind", null));
  });

  it("BACK from 'role' → 'kind' with kind cleared", () => {
    const next = specReducer(s("role", "specialist"), { type: "BACK" });
    expect(next).toEqual(s("kind", null));
  });

  it("BACK from 'generating' → 'role'", () => {
    const next = specReducer(s("generating", "specialist"), { type: "BACK" });
    expect(next).toEqual(s("role", "specialist"));
  });

  it("BACK from 'review' → 'generating'", () => {
    const next = specReducer(s("review", "specialist"), { type: "BACK" });
    expect(next).toEqual(s("generating", "specialist"));
  });

  it("BACK from 'eval' → 'review'", () => {
    const next = specReducer(s("eval", "specialist"), { type: "BACK" });
    expect(next).toEqual(s("review", "specialist"));
  });
});

// ── NEXT ──────────────────────────────────────────────────────────────────────

describe("NEXT transitions (specialist path)", () => {
  it("NEXT from 'role' → 'generating'", () => {
    const next = specReducer(s("role", "specialist"), { type: "NEXT" });
    expect(next).toEqual(s("generating", "specialist"));
  });

  it("NEXT from 'generating' → 'review'", () => {
    const next = specReducer(s("generating", "specialist"), { type: "NEXT" });
    expect(next).toEqual(s("review", "specialist"));
  });

  it("NEXT from 'review' → 'eval'", () => {
    const next = specReducer(s("review", "specialist"), { type: "NEXT" });
    expect(next).toEqual(s("eval", "specialist"));
  });

  it("NEXT on 'eval' is a no-op (terminal)", () => {
    const st = s("eval", "specialist");
    expect(specReducer(st, { type: "NEXT" })).toEqual(st);
  });

  it("NEXT on 'companion' is a no-op (handed off to existing wizard)", () => {
    const st = s("companion", "companion");
    expect(specReducer(st, { type: "NEXT" })).toEqual(st);
  });

  it("NEXT on 'kind' is a no-op", () => {
    expect(specReducer(SPEC_FLOW_INITIAL, { type: "NEXT" })).toEqual(SPEC_FLOW_INITIAL);
  });
});

// ── RESET ─────────────────────────────────────────────────────────────────────

describe("RESET", () => {
  it("resets from any step to initial state", () => {
    const deep = s("eval", "both");
    expect(specReducer(deep, { type: "RESET" })).toEqual(SPEC_FLOW_INITIAL);
  });
});

// ── Full companion flow ───────────────────────────────────────────────────────

describe("Full companion flow path", () => {
  it("kind → companion (hand-off step) via SELECT_KIND", () => {
    let st = SPEC_FLOW_INITIAL;
    st = specReducer(st, { type: "SELECT_KIND", kind: "companion" });
    expect(st.step).toBe("companion");
    expect(st.kind).toBe("companion");
  });
});

// ── Full specialist flow ──────────────────────────────────────────────────────

describe("Full specialist flow path", () => {
  it("kind → role → generating → review → eval", () => {
    let st = SPEC_FLOW_INITIAL;
    st = specReducer(st, { type: "SELECT_KIND", kind: "specialist" });
    expect(st.step).toBe("role");
    st = specReducer(st, { type: "NEXT" });
    expect(st.step).toBe("generating");
    st = specReducer(st, { type: "NEXT" });
    expect(st.step).toBe("review");
    st = specReducer(st, { type: "NEXT" });
    expect(st.step).toBe("eval");
  });
});
