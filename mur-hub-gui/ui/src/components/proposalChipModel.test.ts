import { describe, it, expect } from "vitest";
import { chipReducer, copyText, NO_CHIP, type ChipState, type ProposalChipPayload } from "./proposalChipModel";

const shell: ProposalChipPayload = { agent: "mur", label: "open items", kind: "shell", command: "mur open" };
const restart: ProposalChipPayload = { agent: "mur", label: "apply model", kind: "restart" };

const offer = (chip: ProposalChipPayload, agent = "mur") => ({ type: "offer" as const, chip, agent });

describe("chipReducer", () => {
  it("offers a chip for this agent only", () => {
    expect(chipReducer(NO_CHIP, offer(shell))).toEqual({ phase: "offered", chip: shell });
    expect(chipReducer(NO_CHIP, offer({ ...shell, agent: "other" }))).toBe(NO_CHIP);
  });

  it("a newer offer replaces the older chip", () => {
    const s = chipReducer(chipReducer(NO_CHIP, offer(shell)), offer(restart));
    expect(s).toEqual({ phase: "offered", chip: restart });
  });

  it("dismiss clears an offered chip", () => {
    expect(chipReducer({ phase: "offered", chip: shell }, { type: "dismiss" })).toBe(NO_CHIP);
  });

  it("only restart chips can ask for confirm", () => {
    const offered: ChipState = { phase: "offered", chip: shell };
    expect(chipReducer(offered, { type: "askConfirm" })).toBe(offered);
    expect(chipReducer({ phase: "offered", chip: restart }, { type: "askConfirm" })).toEqual({
      phase: "confirming",
      chip: restart,
    });
  });

  it("restart never starts without passing through confirm", () => {
    const offered: ChipState = { phase: "offered", chip: restart };
    expect(chipReducer(offered, { type: "restartStarted" })).toBe(offered);
  });

  it("cancel returns to offered", () => {
    expect(chipReducer({ phase: "confirming", chip: restart }, { type: "cancelConfirm" })).toEqual({
      phase: "offered",
      chip: restart,
    });
  });

  it("confirm → restarting → done clears the chip", () => {
    let s: ChipState = { phase: "confirming", chip: restart };
    s = chipReducer(s, { type: "restartStarted" });
    expect(s.phase).toBe("restarting");
    expect(chipReducer(s, { type: "restartDone" })).toBe(NO_CHIP);
  });

  it("a failed restart keeps the chip with the error", () => {
    const s = chipReducer({ phase: "restarting", chip: restart }, { type: "restartFailed", error: "boom" });
    expect(s).toEqual({ phase: "failed", chip: restart, error: "boom" });
  });

  it("an in-flight restart ignores new offers and dismiss", () => {
    const busy: ChipState = { phase: "restarting", chip: restart };
    expect(chipReducer(busy, offer(shell))).toBe(busy);
    expect(chipReducer(busy, { type: "dismiss" })).toBe(busy);
  });
});

describe("copyText", () => {
  it("copies the command for shell and slash, nothing for restart", () => {
    expect(copyText(shell)).toBe("mur open");
    expect(copyText({ agent: "mur", label: "m", kind: "slash", command: "/model" })).toBe("/model");
    expect(copyText(restart)).toBeNull();
  });
});
