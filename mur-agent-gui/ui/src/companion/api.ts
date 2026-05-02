import { invoke, Channel } from "@tauri-apps/api/core";
import type { BridgeEvent, Signal } from "./types";

export async function listPending(agent: string): Promise<BridgeEvent[]> {
  return await invoke<BridgeEvent[]>("companion_bridge_pending", { agent });
}

export async function subscribe(
  agent: string,
  onEvent: (ev: BridgeEvent) => void,
): Promise<void> {
  const channel = new Channel<BridgeEvent>();
  channel.onmessage = onEvent;
  await invoke("companion_bridge_subscribe", { agent, onEvent: channel });
}

export async function ack(
  agent: string,
  msgId: string,
  signal: Signal,
): Promise<void> {
  await invoke("companion_ack", { agent, msgId, signal });
}

export async function notifyMessage(
  situation: string,
  body: string,
): Promise<void> {
  await invoke("notify_message", { situation, body });
}

export async function setBadge(count: number): Promise<void> {
  await invoke("set_unread_badge", { count });
}
