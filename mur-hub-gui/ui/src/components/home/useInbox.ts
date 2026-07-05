import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useAgents } from "../../context/AgentContext";
import { mergeInbox, type InboxItem } from "./inbox";

const POLL_INTERVAL_MS = 30_000;

/** Raw shape of `hitl_pending_list()` items (`HitlRequestView`, hitl.rs). */
export interface RawHitl {
  channel_id: string;
  hitl_id: string;
  agent: string;
  summary: string;
  risk: string;
  ts: string;
}

/** Raw shape of `install_inbox_list()` items (`InstallRequestView`, install_inbox.rs). */
export interface RawInstall {
  install_type: string;
  id: string;
  publisher: string;
  request_id: string;
  requested_at: number;
  is_official: boolean;
}

/** Raw shape of `companion_bridge_pending({ agent })` items (`BridgeEvent`, CompanionInbox.tsx). */
export interface RawCompanionEvent {
  id: string;
  situation: string;
  template_id: string;
  locale: string;
  generated_at: string;
  body: string;
  response: { kind: "unset" | "signal"; value?: string };
}

/** Raw shape of `skill_upgrade_status()` items (`BlockedItem`, upgrade_status.rs). */
export interface RawBlockedItem {
  name: string;
  dir: string;
  local_version: string;
  latest_version: string;
}

function parseTs(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string") {
    const asNum = Number(value);
    if (Number.isFinite(asNum) && value.trim() !== "") return asNum;
    const parsed = Date.parse(value);
    if (!Number.isNaN(parsed)) return Math.floor(parsed / 1000);
  }
  return null;
}

/** Pure adapter: `RawHitl` -> `InboxItem`. Returns null on malformed input (fail-open). */
export function hitlToItem(raw: RawHitl): InboxItem | null {
  if (!raw || typeof raw !== "object") return null;
  const { channel_id, hitl_id, agent, summary, risk, ts } = raw;
  if (
    typeof channel_id !== "string" ||
    !channel_id ||
    typeof hitl_id !== "string" ||
    !hitl_id ||
    typeof agent !== "string" ||
    typeof summary !== "string" ||
    typeof risk !== "string"
  ) {
    return null;
  }
  const tsValue = parseTs(ts);
  if (tsValue === null) return null;
  return {
    kind: "hitl",
    id: `${channel_id}:${hitl_id}`,
    ts: tsValue,
    title: `${agent}: approval needed`,
    subtitle: summary || `Risk: ${risk}`,
    payload: raw,
  };
}

/** Pure adapter: `RawInstall` -> `InboxItem`. Returns null on malformed input (fail-open). */
export function installToItem(raw: RawInstall): InboxItem | null {
  if (!raw || typeof raw !== "object") return null;
  const { install_type, id, publisher, requested_at } = raw;
  if (
    typeof id !== "string" ||
    !id ||
    typeof install_type !== "string" ||
    typeof publisher !== "string"
  ) {
    return null;
  }
  const tsValue = parseTs(requested_at);
  if (tsValue === null) return null;
  return {
    kind: "install",
    id,
    ts: tsValue,
    title: `Install request: ${install_type}`,
    subtitle: `From ${publisher}`,
    payload: raw,
  };
}

/** Pure adapter: `RawCompanionEvent` -> `InboxItem`. Returns null on malformed input (fail-open). */
export function companionToItem(raw: RawCompanionEvent): InboxItem | null {
  if (!raw || typeof raw !== "object") return null;
  const { id, situation, body, generated_at } = raw;
  if (typeof id !== "string" || !id || typeof body !== "string") return null;
  const tsValue = parseTs(generated_at);
  if (tsValue === null) return null;
  return {
    kind: "companion",
    id,
    ts: tsValue,
    title: situation && typeof situation === "string" ? situation : "MUR companion message",
    subtitle: body,
    payload: raw,
  };
}

/** Pure adapter: `RawBlockedItem` -> `InboxItem`. Returns null on malformed input (fail-open). */
export function blockedToItem(raw: RawBlockedItem): InboxItem | null {
  if (!raw || typeof raw !== "object") return null;
  const { name, local_version, latest_version } = raw;
  if (
    typeof name !== "string" ||
    !name ||
    typeof local_version !== "string" ||
    typeof latest_version !== "string"
  ) {
    return null;
  }
  return {
    kind: "upgrade_blocked",
    id: name,
    ts: 0,
    title: `Skill "${name}" upgrade blocked`,
    subtitle: `Local ${local_version} modified; latest is ${latest_version}`,
    payload: raw,
  };
}

function filterNulls<T>(items: (T | null)[]): T[] {
  return items.filter((item): item is T => item !== null);
}

/**
 * Aggregates the four inbox sources (HITL approvals, install requests,
 * companion bridge messages, blocked skill upgrades) into one sorted,
 * deduplicated `InboxItem[]`. Per-source failures are swallowed so one
 * broken source doesn't blank the others.
 */
export function useInbox(): { items: InboxItem[]; refresh: () => void } {
  const { agents } = useAgents();
  const [hitlItems, setHitlItems] = useState<InboxItem[]>([]);
  const [installItems, setInstallItems] = useState<InboxItem[]>([]);
  const [companionItems, setCompanionItems] = useState<InboxItem[]>([]);
  const [blockedItems, setBlockedItems] = useState<InboxItem[]>([]);
  const agentNamesRef = useRef<string[]>([]);
  agentNamesRef.current = agents.map((a) => a.name);

  const refreshHitl = useCallback(() => {
    invoke<RawHitl[]>("hitl_pending_list")
      .then((raw) => setHitlItems(filterNulls(raw.map(hitlToItem))))
      .catch(() => {});
  }, []);

  const refreshInstall = useCallback(() => {
    invoke<RawInstall[]>("install_inbox_list")
      .then((raw) => setInstallItems(filterNulls(raw.map(installToItem))))
      .catch(() => {});
  }, []);

  const refreshCompanion = useCallback(() => {
    Promise.all(
      agentNamesRef.current.map((agent) =>
        invoke<RawCompanionEvent[]>("companion_bridge_pending", { agent }).catch(() => []),
      ),
    )
      .then((perAgent) => setCompanionItems(filterNulls(perAgent.flat().map(companionToItem))))
      .catch(() => {});
  }, []);

  const refreshBlocked = useCallback(() => {
    invoke<RawBlockedItem[]>("skill_upgrade_status")
      .then((raw) => setBlockedItems(filterNulls(raw.map(blockedToItem))))
      .catch(() => {});
  }, []);

  const refresh = useCallback(() => {
    refreshHitl();
    refreshInstall();
    refreshCompanion();
    refreshBlocked();
  }, [refreshHitl, refreshInstall, refreshCompanion, refreshBlocked]);

  useEffect(() => {
    refresh();

    const unlistenHitl = listen("hitl-approval-needed", () => refreshHitl());
    const interval = setInterval(() => {
      refreshInstall();
      refreshBlocked();
    }, POLL_INTERVAL_MS);

    return () => {
      unlistenHitl.then((fn) => fn()).catch(() => {});
      clearInterval(interval);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    refreshCompanion();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agents.length]);

  const items = mergeInbox([hitlItems, installItems, companionItems, blockedItems]);

  return { items, refresh };
}
