import { describe, expect, it } from "vitest";
import { initialChoices, notReadyText, pairStateText, pendingToItem, verdictKey } from "./sshModel";
import type { PendingView, PlanView } from "./sshApi";

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
    });
  });
});
