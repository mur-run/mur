//! Display helpers. No SSH rule: whether a choice may be approved is
//! `agent_ssh_check`'s answer, never decided here.

import { translate } from "../../i18n";
import type { TranslationKey } from "../../i18n/types";
import type { InboxItem } from "../home/inbox";
import type { Choices, HostRuleView, NotReady, PendingView, PlanView, VerdictName } from "./sshApi";

type T = (k: TranslationKey, v?: Record<string, string | number>) => string;

/** `ssh_pending_all()` row → "Needs you" item. Null on malformed input. */
export function pendingToItem(raw: PendingView, lang: string): InboxItem | null {
  if (
    !raw ||
    typeof raw !== "object" ||
    typeof raw.agent !== "string" ||
    typeof raw.host !== "string" ||
    typeof raw.port !== "number" ||
    typeof raw.canonical !== "string" ||
    typeof raw.reason !== "string" ||
    typeof raw.requested_at !== "number"
  ) {
    return null;
  }
  return {
    kind: "ssh_request",
    id: `${raw.agent}:${raw.canonical}`,
    ts: raw.requested_at,
    title: translate(lang, "ssh.inbox.title", { agent: raw.agent, host: raw.canonical }),
    subtitle: raw.reason,
    payload: raw,
    agent: raw.agent,
  };
}

export function verdictKey(v: VerdictName): TranslationKey {
  return `ssh.verdict.${v}`;
}

export function notReadyText(n: NotReady, t: T): string {
  switch (n.kind) {
    case "host":
    case "user":
      return n.detail;
    default:
      return t(`ssh.notReady.${n.kind}`);
  }
}

function when(iso: string): string {
  return new Date(iso).toLocaleString();
}

export function pairStateText(h: HostRuleView, t: T): string {
  if (h.state === "signed" && h.signed_at) return t("ssh.state.signed", { at: when(h.signed_at) });
  return t(`ssh.state.${h.state === "signed" ? "approved" : h.state}`);
}

export function expiredMarkText(h: HostRuleView, t: T): string | null {
  return h.last_expired_mark ? t("ssh.expiredMark", { at: when(h.last_expired_mark) }) : null;
}

/** Where the card starts. A default only; the backend still checks it. */
export function initialChoices(plan: PlanView): Choices {
  const first = plan.keys.find((k) => k.loaded) ?? plan.keys[0];
  return {
    key_fp: first?.fingerprint ?? null,
    pasted: null,
    ticked: [],
    spawn_consented: false,
    user: plan.user ?? "",
  };
}
