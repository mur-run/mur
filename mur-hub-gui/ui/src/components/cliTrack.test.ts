import { describe, it, expect } from "vitest";
import { trackOffer, type CliGate } from "./cliTrack";
import type { HookState } from "./chatgptSubscription";

describe("the selection table, row by row", () => {
  it("hook true: the gateway is the track, whatever the CLI is doing", () => {
    for (const cli of ["absent", "passed", "failed"] as CliGate[]) {
      expect(trackOffer("true", cli)).toEqual({ gateway: "preferred", cliSpawn: "none" });
    }
  });

  it("hook unknown, no CLI: gateway CTA", () => {
    expect(trackOffer("unknown", "absent")).toEqual({ gateway: "cta", cliSpawn: "none" });
  });

  it("hook unknown, CLI passed: CTA first, CLI available but not preselected", () => {
    expect(trackOffer("unknown", "passed")).toEqual({ gateway: "cta", cliSpawn: "available" });
  });

  it("hook unknown, CLI failed: CTA still live, CLI disabled", () => {
    expect(trackOffer("unknown", "failed")).toEqual({ gateway: "cta", cliSpawn: "disabled" });
  });

  it("hook false, CLI passed: CLI spawn is the track", () => {
    expect(trackOffer("false", "passed")).toEqual({ gateway: "none", cliSpawn: "offered" });
  });

  it("hook false, CLI failed: both off, the CLI panel names what is unmet", () => {
    expect(trackOffer("false", "failed")).toEqual({ gateway: "none", cliSpawn: "disabled" });
  });

  it("hook false, no CLI: neither track exists", () => {
    expect(trackOffer("false", "absent")).toEqual({ gateway: "none", cliSpawn: "none" });
  });
});

describe("the invariants the table exists to protect", () => {
  it("unknown never disables a control", () => {
    // Every unknown row keeps the gateway CTA live. This is the #1334 lesson:
    // the control that reflects availability is the button, not visibility.
    for (const cli of ["absent", "passed", "failed"] as CliGate[]) {
      expect(trackOffer("unknown", cli).gateway).toBe("cta");
    }
  });

  it("unknown never routes silently to CLI spawn", () => {
    // "available" is reachable; "offered" — the preselected track — is not.
    for (const cli of ["absent", "passed", "failed"] as CliGate[]) {
      expect(trackOffer("unknown", cli).cliSpawn).not.toBe("offered");
    }
  });

  it("unknown is never folded into false", () => {
    // If any CLI gate produced the same answer for both, the distinction the
    // whole tri-state exists for would have collapsed.
    for (const cli of ["absent", "passed", "failed"] as CliGate[]) {
      expect(trackOffer("unknown", cli)).not.toEqual(trackOffer("false", cli));
    }
  });

  it("every hook state answers for every CLI gate", () => {
    for (const hook of ["true", "false", "unknown"] as HookState[]) {
      for (const cli of ["absent", "passed", "failed"] as CliGate[]) {
        expect(trackOffer(hook, cli)).toBeTruthy();
      }
    }
  });
});
