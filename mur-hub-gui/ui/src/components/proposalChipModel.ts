// Pure state for the `propose` chip (#1566), kept out of the component so the
// transitions are unit-testable without a DOM.
//
// Hub rules (mirrors murmur where the Hub can, diverges where it can't):
// - One chip per conversation; a newer proposal replaces the older one.
// - Sending a message, Esc, or switching agent clears it.
// - shell / slash chips are COPY-only: the Hub composer has no `!`/`/` mode,
//   so inserting the command would just send it to the agent as chat.
// - restart is the only actionable kind, and only behind an explicit confirm.
// - If the Hub ever grows a reply ghost, it must never displace a chip.

/** Payload of the `proposal-offered` Tauri event (see `proposal_chip.rs`). */
export interface ProposalChipPayload {
  agent: string;
  label: string;
  kind: "shell" | "slash" | "restart";
  /** Absent for `restart`. */
  command?: string;
}

/** Tauri event name; must match `PROPOSAL_EVENT` in `proposal_chip.rs`. */
export const PROPOSAL_EVENT = "proposal-offered";

export type ChipState =
  | { phase: "none" }
  | { phase: "offered"; chip: ProposalChipPayload }
  | { phase: "confirming"; chip: ProposalChipPayload }
  | { phase: "restarting"; chip: ProposalChipPayload }
  | { phase: "failed"; chip: ProposalChipPayload; error: string };

export type ChipAction =
  | { type: "offer"; chip: ProposalChipPayload; agent: string }
  | { type: "dismiss" }
  | { type: "askConfirm" }
  | { type: "cancelConfirm" }
  | { type: "restartStarted" }
  | { type: "restartDone" }
  | { type: "restartFailed"; error: string };

export const NO_CHIP: ChipState = { phase: "none" };

export function chipReducer(state: ChipState, action: ChipAction): ChipState {
  switch (action.type) {
    case "offer":
      // Another agent's proposal never lands in this conversation.
      if (action.chip.agent !== action.agent) return state;
      // A restart in flight is not interrupted by a newer offer.
      if (state.phase === "restarting") return state;
      return { phase: "offered", chip: action.chip };
    case "dismiss":
      return state.phase === "restarting" ? state : NO_CHIP;
    case "askConfirm":
      return state.phase === "offered" && state.chip.kind === "restart"
        ? { phase: "confirming", chip: state.chip }
        : state;
    case "cancelConfirm":
      return state.phase === "confirming" ? { phase: "offered", chip: state.chip } : state;
    case "restartStarted":
      return state.phase === "confirming" ? { phase: "restarting", chip: state.chip } : state;
    case "restartDone":
      return state.phase === "restarting" ? NO_CHIP : state;
    case "restartFailed":
      return state.phase === "restarting"
        ? { phase: "failed", chip: state.chip, error: action.error }
        : state;
  }
}

/** Text a copy button puts on the clipboard; `null` when there is nothing to copy. */
export function copyText(chip: ProposalChipPayload): string | null {
  return chip.kind === "restart" ? null : (chip.command ?? null);
}
