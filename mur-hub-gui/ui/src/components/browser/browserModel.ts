//! Display helpers. No browser rule: which row may be used, and why not, is
//! `agent_browser_card`'s answer, never decided here.

import { translate } from "../../i18n";
import type { InboxItem } from "../home/inbox";
import type { PendingView } from "./browserApi";

/** `agent_browser_pending` row → "Needs you" item. Null on malformed input. */
export function pendingToItem(raw: PendingView, lang: string): InboxItem | null {
  if (
    !raw ||
    typeof raw !== "object" ||
    typeof raw.agent !== "string" ||
    typeof raw.site !== "string" ||
    typeof raw.reason !== "string" ||
    typeof raw.requested_at !== "string"
  ) {
    return null;
  }
  const ms = Date.parse(raw.requested_at);
  return {
    kind: "browser_request",
    id: `${raw.agent}:${raw.site}`,
    ts: Number.isNaN(ms) ? 0 : Math.floor(ms / 1000),
    title: translate(lang, raw.login ? "browser.inbox.titleLogin" : "browser.inbox.title", {
      agent: raw.agent,
      site: raw.site,
    }),
    subtitle: raw.reason,
    payload: raw,
    agent: raw.agent,
  };
}
