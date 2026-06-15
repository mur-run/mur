/**
 * specFlow.ts — Pure state machine for the agent-wizard "Specialist" flow (Plan 4).
 *
 * The Companion flow (Steps 1–6) is UNCHANGED. This file only governs Step 0 (kind
 * selection) and the subsequent routing:
 *
 *   Companion  → existing wizard steps 1–6 (no change)
 *   Specialist → Plan 4 steps: Role → Generating → Review → Eval   (T5–T6)
 *   Both       → same as Specialist (both roles coexist at runtime)
 *
 * `specReducer` is a pure function: (SpecFlowState, SpecFlowEvent) → SpecFlowState.
 * It has no side-effects and is fully unit-testable with Vitest.
 */

// ── Types ─────────────────────────────────────────────────────────────────────

export type AgentKind = "companion" | "specialist" | "both";

export type SpecFlowStep =
  | "kind"          // Step 0: Companion / Specialist / Both fork
  | "companion"     // Hand-off to existing wizard steps 1–6
  | "role"          // Specialist Step 1: Role selection (T5)
  | "generating"    // Specialist Step 2: LLM generation in-progress (T5)
  | "review"        // Specialist Step 3: Review generated draft (T6)
  | "eval";         // Specialist Step 4: Eval scores (T6)

export interface SpecFlowState {
  step: SpecFlowStep;
  kind: AgentKind | null;
}

export type SpecFlowEvent =
  | { type: "SELECT_KIND"; kind: AgentKind }
  | { type: "BACK" }
  | { type: "NEXT" }   // advance through specialist steps
  | { type: "RESET" };

// ── Initial state ─────────────────────────────────────────────────────────────

export const SPEC_FLOW_INITIAL: SpecFlowState = {
  step: "kind",
  kind: null,
};

// ── Reducer ───────────────────────────────────────────────────────────────────

/**
 * Pure state-machine reducer for the wizard flow.
 *
 * SELECT_KIND on "kind" step → route to "companion" or "role" depending on kind.
 * BACK → return to the previous step (or stay at "kind").
 * NEXT → advance through specialist steps.
 * RESET → return to initial state.
 */
export function specReducer(
  state: SpecFlowState,
  event: SpecFlowEvent,
): SpecFlowState {
  switch (event.type) {
    case "SELECT_KIND": {
      const kind = event.kind;
      if (state.step !== "kind") return state;
      const next: SpecFlowStep =
        kind === "companion" ? "companion" : "role";
      return { step: next, kind };
    }

    case "BACK": {
      switch (state.step) {
        case "kind":
          return state; // already at the start
        case "companion":
          return { ...state, step: "kind", kind: null };
        case "role":
          return { ...state, step: "kind", kind: null };
        case "generating":
          return { ...state, step: "role" };
        case "review":
          return { ...state, step: "generating" };
        case "eval":
          return { ...state, step: "review" };
        default:
          return state;
      }
    }

    case "NEXT": {
      switch (state.step) {
        case "role":
          return { ...state, step: "generating" };
        case "generating":
          return { ...state, step: "review" };
        case "review":
          return { ...state, step: "eval" };
        // "companion" and "eval" do not NEXT further via this reducer
        default:
          return state;
      }
    }

    case "RESET":
      return { ...SPEC_FLOW_INITIAL };

    default:
      return state;
  }
}
