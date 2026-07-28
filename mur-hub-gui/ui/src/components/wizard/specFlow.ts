/**
 * specFlow.ts — Pure state machine for the agent-creation wizard.
 *
 * Step 0 asks where the agent comes from, not what "kind" it is. The old
 * companion / specialist / both fork conflated two independent things — what an
 * agent DOES (role, skills, prompt) and what it LOOKS like — and only the
 * specialist branch ever produced a working agent (the companion branch never
 * wrote a profile.yaml). Appearance is now a step every new agent passes
 * through, so any agent can be a desktop pet.
 *
 *   template → Role → Generating → Review → Eval → Appearance
 *   official → Catalog (browse + install)        → Appearance
 *   import   → handed to MuragentImportModal (the wizard closes)
 *
 * `specReducer` is a pure function: (SpecFlowState, SpecFlowEvent) → SpecFlowState.
 * It has no side-effects and is fully unit-testable with Vitest.
 */

// ── Types ─────────────────────────────────────────────────────────────────────

/** Where the new agent comes from. */
export type AgentSource = "template" | "official" | "import";

export type SpecFlowStep =
  | "source"        // Step 0: where does this agent come from?
  | "role"          // Template flow: role selection
  | "generating"    // Template flow: LLM generation in-progress
  | "review"        // Template flow: review generated draft
  | "eval"          // Template flow: eval scores
  | "official"      // Official flow: browse + install a catalog item
  | "appearance";   // Shared final step: give the new agent a pet look

export interface SpecFlowState {
  step: SpecFlowStep;
  source: AgentSource | null;
}

export type SpecFlowEvent =
  | { type: "SELECT_SOURCE"; source: AgentSource }
  | { type: "BACK" }
  | { type: "NEXT" }
  | { type: "RESET" };

// ── Initial state ─────────────────────────────────────────────────────────────

export const SPEC_FLOW_INITIAL: SpecFlowState = {
  step: "source",
  source: null,
};

// ── Reducer ───────────────────────────────────────────────────────────────────

/**
 * Pure state-machine reducer for the wizard flow.
 *
 * SELECT_SOURCE on "source" → route into that source's first step. "import" is
 * handled by the host (it opens the .muragent import modal), so it records the
 * source and stays put while the host closes the wizard.
 * BACK → previous step, never past a point where the agent already exists.
 * NEXT → advance; both creating flows converge on "appearance".
 */
export function specReducer(
  state: SpecFlowState,
  event: SpecFlowEvent,
): SpecFlowState {
  switch (event.type) {
    case "SELECT_SOURCE": {
      if (state.step !== "source") return state;
      const source = event.source;
      switch (source) {
        case "template":
          return { step: "role", source };
        case "official":
          return { step: "official", source };
        case "import":
          // The host opens the import modal and closes the wizard — no step of
          // our own to move to.
          return { ...state, source };
      }
      return state;
    }

    case "BACK": {
      switch (state.step) {
        case "source":
          return state; // already at the start
        case "role":
        case "official":
          return { step: "source", source: null };
        case "generating":
          return { ...state, step: "role" };
        case "review":
          return { ...state, step: "generating" };
        case "eval":
        case "appearance":
          // Terminal: the agent already exists on disk. Going BACK to
          // re-approve a consumed draft would error. No-op.
          return state;
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
        case "eval":
        case "official":
          // Every created agent gets the appearance offer.
          return { ...state, step: "appearance" };
        // "appearance" is terminal — the host closes the wizard.
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
