// Mirrors mur-agent-gui/src-tauri/src/companion_bridge/event.rs.

export type Signal = "good" | "bad" | "dismiss";

export type BridgeResponse =
  | { kind: "unset" }
  | { kind: "signal"; value: Signal };

export interface BridgeEvent {
  id: string;
  situation: string;
  template_id: string;
  locale: string;
  generated_at: string;
  body: string;
  response: BridgeResponse;
}
