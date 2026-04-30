// Mirrors the Rust types in mur-agent-gui/src-tauri/src/voice/.

export interface InstalledVoice {
  voice_id: string;
  display_name: string;
  language: string;
  sample_rate_hz: number;
  license: string;
  installed_at: string; // RFC 3339
}

export interface VoiceListResponse {
  voices: InstalledVoice[];
  default_voice_id: string | null;
}

export interface VoiceStatus {
  enabled: boolean;
  default_voice_id: string | null;
  voices_installed: number;
  stt_installed: boolean;
  stt_loaded: boolean;
}

export interface SttStatus {
  model_id: string;
  installed: boolean;
  loaded: boolean;
  size_bytes: number;
}

// Variants of the DownloadProgress enum from voice/download.rs.
// Tagged via `#[serde(tag = "kind")]` on the Rust side.
export type DownloadProgress =
  | { kind: "ManifestFetched" }
  | { kind: "ManifestVerified" }
  | { kind: "AssetStarted"; name: string; size_bytes: number }
  | {
      kind: "AssetProgress";
      name: string;
      downloaded_bytes: number;
      total_bytes: number;
    }
  | { kind: "AssetComplete"; name: string }
  | { kind: "Done" };

export interface HotkeyConfig {
  modifiers: string[]; // subset of: super, control, alt, shift
  code: string; // KeyboardEvent.code, e.g. "Quote", "KeyM"
}
