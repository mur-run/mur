import { describe, expect, it } from "vitest";
import { choicesToApply, dedicatedChosen, initialChoices, notReadyText, pairStateText, pendingToItem, resultState, selectDedicated, verdictKey } from "./sshModel";
import type { ApplyView, PendingView, PlanView } from "./sshApi";

const pending: PendingView = {
  agent: "quill",
  host: "gh.example",
  port: 22,
  canonical: "gh.example:22",
  user: "git",
  reason: "deploy the site",
  requested_at: 1_800_000_000,
  granted: false,
};

describe("pendingToItem", () => {
  it("maps a request to a fifth-kind inbox item", () => {
    const it2 = pendingToItem(pending, "en");
    expect(it2?.kind).toBe("ssh_request");
    expect(it2?.id).toBe("quill:gh.example:22");
    expect(it2?.agent).toBe("quill");
    expect(it2?.ts).toBe(1_800_000_000);
    expect(it2?.title).toContain("gh.example:22");
    expect(it2?.subtitle).toBe("deploy the site");
  });
  it("drops malformed input", () => {
    expect(pendingToItem({ ...pending, port: "22" as unknown as number }, "en")).toBeNull();
    expect(pendingToItem(null as unknown as PendingView, "en")).toBeNull();
  });
});

describe("texts", () => {
  const t = (k: string, v?: Record<string, string | number>) => `${k}${v ? JSON.stringify(v) : ""}`;
  it("names the verdict key", () => {
    expect(verdictKey("unknown_downgraded")).toBe("ssh.verdict.unknown_downgraded");
  });
  it("shows the backend's words for host and user, keys for the rest", () => {
    expect(notReadyText({ kind: "host", detail: "did not offer X" }, t)).toBe("did not offer X");
    expect(notReadyText({ kind: "spawn_consent" }, t)).toBe("ssh.notReady.spawn_consent");
  });
  it("only a signed pair carries a time", () => {
    const h = { host_key: "h", label: null, names: [], state: "approved" as const, signed_at: null, last_expired_mark: null };
    expect(pairStateText(h, t)).toBe("ssh.state.approved");
    expect(pairStateText({ ...h, state: "signed", signed_at: "2026-11-14T00:00:00Z" }, t)).toContain("ssh.state.signed");
  });
});

describe("initialChoices", () => {
  it("starts on the first loaded key and the request's user", () => {
    const plan = {
      keys: [
        { fingerprint: "U", comment: "", loaded: false, add_cmd: "ssh-add u" },
        { fingerprint: "K", comment: "", loaded: true, add_cmd: null },
      ],
      user: "git",
    } as unknown as PlanView;
    expect(initialChoices(plan)).toEqual({
      key_fp: "K",
      pasted: null,
      ticked: [],
      spawn_consented: false,
      user: "git",
      install: false,
    });
  });
});

describe("dedicated key", () => {
  const base = {
    keys: [
      { fingerprint: "K", comment: "", loaded: true, add_cmd: null },
      { fingerprint: "D", comment: "", loaded: true, add_cmd: null },
    ],
    user: "git",
    dedicated: { state: "stored", reason: null, fingerprint: "D", line: "ssh-ed25519 AAAA mur" },
  } as unknown as PlanView;

  it("the install shows only for the dedicated key", () => {
    const c = initialChoices(base);
    expect(c.install).toBe(false);
    expect(dedicatedChosen(base, c)).toBe(false);
    expect(dedicatedChosen(base, selectDedicated(base, c))).toBe(true);
  });
  it("a hidden install tick is never sent", () => {
    const ticked = { ...selectDedicated(base, initialChoices(base)), install: true };
    expect(choicesToApply(base, ticked).install).toBe(true);
    // Switched to another key: the checkbox is hidden, the tick stays in state.
    expect(choicesToApply(base, { ...ticked, key_fp: "K" }).install).toBe(false);
    // User cleared: the checkbox is disabled.
    expect(choicesToApply(base, { ...ticked, user: "" }).install).toBe(false);
    // No line to install.
    const noLine = { ...base, dedicated: { ...base.dedicated, line: null } } as PlanView;
    expect(choicesToApply(noLine, ticked).install).toBe(false);
    // Everything else is passed through untouched.
    expect(choicesToApply(base, { ...ticked, key_fp: "K" }).key_fp).toBe("K");
  });
  it("selects nothing that is not in the key list", () => {
    const p = { ...base, dedicated: { ...base.dedicated, fingerprint: "Z" } } as PlanView;
    expect(selectDedicated(p, initialChoices(p)).key_fp).toBe("K");
  });
  it("maps the apply view to the card's result state", () => {
    const r = {
      lines: [],
      restarted: false,
      can_restart: false,
      hand_back: null,
      install: { state: "failed", line: "l" },
      held: true,
      advisory: true,
    } as ApplyView;
    expect(resultState(r)).toBe("held");
    expect(resultState({ ...r, held: false, restarted: true, install: { state: "not_chosen", line: "l" } })).toBe("manual");
    expect(resultState({ ...r, held: false, install: { state: "done", line: "l" } })).toBe("closed");
    expect(resultState({ ...r, held: false, advisory: false, install: { state: "not_chosen", line: null } })).toBe("closed");
  });
});
