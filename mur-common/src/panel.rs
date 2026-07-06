//! Wire protocol + on-disk session records for the murmur Panel (companion
//! window). Shared by the murmur TUI (socket server side) and
//! `mur-gui-core::panel_bridge` (Hub client side). One JSON object per line.
//! Design: docs/superpowers/specs/2026-07-05-murmur-panel-companion-design.md

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const PANEL_PROTO_VERSION: u32 = 2;

/// Debounce for `InputChanged` snapshots (murmur side). ~200 ms is the
/// autocomplete sweet spot; >300 ms degrades UX (Algolia guidance).
pub const INPUT_DEBOUNCE_MS: u64 = 200;
/// Below this many trimmed chars the Hub falls back to cwd recommendations.
pub const MIN_QUERY_CHARS: usize = 2;
/// Snapshot size bound; the tail (what the user is typing now) is kept.
pub const INPUT_SNAPSHOT_MAX_CHARS: usize = 2000;

/// Directory holding one `<pid>.json` + `<pid>.sock` per live murmur session.
/// Under the existing `~/.mur/runtime` convention (see `media::runtime_dir`).
pub fn murmur_run_dir(mur_home: &Path) -> PathBuf {
    mur_home.join("runtime").join("murmur")
}

/// On-disk session record (`<pid>.json`) and `Hello` payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanelSession {
    pub pid: u32,
    pub agent: String,
    pub cwd: String,
    pub sock: String,
    #[serde(default)]
    pub terminal_program: Option<String>,
    pub proto_version: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelTab {
    Information,
    Activities,
    Preview,
    Notifications,
    Schedule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewKind {
    File,
    Url,
}

/// murmur → Hub frames.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PanelFrame {
    /// Sent once per connection, immediately on accept.
    Hello {
        session: PanelSession,
    },
    /// Reserved for later phases (cwd/agent changes mid-session).
    State {
        cwd: String,
        agent: String,
    },
    /// `/panel <tab>` — open/focus the Panel window on a tab.
    Panel {
        focus: PanelTab,
    },
    /// `/panel preview <target>` — set preview target (rendered in P3).
    Preview {
        kind: PreviewKind,
        target: String,
    },
    /// Live agent-output delta (P4; sent only while the session gate is on).
    Stream {
        delta: String,
    },
    /// Debounced snapshot of the message input line (never per-keystroke
    /// deltas). Local-socket only; must never be persisted or forwarded
    /// (spec §3.2). Build the payload with [`input_snapshot`].
    InputChanged {
        text: String,
    },
    Bye,
}

/// Hub → murmur frames. Insert-only by design: nothing here may ever
/// execute — `Insert` fills the input box and the user presses Enter
/// (fail-closed; see spec security section).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubFrame {
    Insert { text: String },
}

/// Tolerant single-line decode: unknown/malformed frames are `None`
/// (forward compatibility across proto versions).
pub fn decode_line<T: serde::de::DeserializeOwned>(line: &str) -> Option<T> {
    serde_json::from_str(line).ok()
}

/// Build a privacy-safe `InputChanged` payload: redact secret-looking
/// tokens, then keep only the trailing `INPUT_SNAPSHOT_MAX_CHARS` chars
/// (the tail is what the user is currently typing).
pub fn input_snapshot(text: &str) -> String {
    let redacted: Vec<String> = text
        .split_whitespace()
        .map(|tok| {
            if looks_secret(tok) {
                "[redacted]".to_string()
            } else {
                tok.to_string()
            }
        })
        .collect();
    let joined = redacted.join(" ");
    let n = joined.chars().count();
    if n <= INPUT_SNAPSHOT_MAX_CHARS {
        joined
    } else {
        joined.chars().skip(n - INPUT_SNAPSHOT_MAX_CHARS).collect()
    }
}

/// Conservative secret heuristics: `sk-` API keys, AWS `AKIA` key ids,
/// and long (32+) unbroken hex/base64-ish runs.
fn looks_secret(tok: &str) -> bool {
    let is_b64ish = |s: &str| {
        !s.is_empty()
            && s.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || c == '+'
                    || c == '/'
                    || c == '='
                    || c == '_'
                    || c == '-'
            })
    };
    if let Some(rest) = tok.strip_prefix("sk-")
        && rest.len() >= 16
        && is_b64ish(rest)
    {
        return true;
    }
    if let Some(rest) = tok.strip_prefix("AKIA")
        && rest.len() == 16
        && rest
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return true;
    }
    tok.len() >= 32 && is_b64ish(tok) && tok.chars().any(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn run_dir_under_runtime() {
        assert_eq!(
            murmur_run_dir(Path::new("/h")),
            Path::new("/h/runtime/murmur")
        );
    }

    #[test]
    fn frames_round_trip() {
        let s = PanelSession {
            pid: 42,
            agent: "mur".into(),
            cwd: "/tmp".into(),
            sock: "/h/runtime/murmur/42.sock".into(),
            terminal_program: Some("iTerm.app".into()),
            proto_version: PANEL_PROTO_VERSION,
        };
        let line = serde_json::to_string(&PanelFrame::Hello { session: s }).unwrap();
        assert!(line.contains("\"type\":\"hello\""));
        assert!(matches!(
            decode_line::<PanelFrame>(&line),
            Some(PanelFrame::Hello { .. })
        ));

        let line = serde_json::to_string(&PanelFrame::Panel {
            focus: PanelTab::Preview,
        })
        .unwrap();
        assert!(line.contains("\"focus\":\"preview\""));

        let line = serde_json::to_string(&PanelFrame::Panel {
            focus: PanelTab::Schedule,
        })
        .unwrap();
        assert!(line.contains("\"focus\":\"schedule\""));

        let line = serde_json::to_string(&PanelFrame::Stream {
            delta: "tok".into(),
        })
        .unwrap();
        assert!(line.contains("\"type\":\"stream\""));
        assert!(matches!(
            decode_line::<PanelFrame>(&line),
            Some(PanelFrame::Stream { delta }) if delta == "tok"
        ));

        let line = serde_json::to_string(&HubFrame::Insert {
            text: "/help".into(),
        })
        .unwrap();
        assert!(matches!(
            decode_line::<HubFrame>(&line),
            Some(HubFrame::Insert { text }) if text == "/help"
        ));
    }

    #[test]
    fn unknown_frames_are_none() {
        assert!(decode_line::<PanelFrame>(r#"{"type":"from_the_future"}"#).is_none());
        assert!(decode_line::<HubFrame>(r#"{"type":"execute","cmd":"rm"}"#).is_none());
        assert!(decode_line::<HubFrame>("not json").is_none());
    }

    #[test]
    fn input_changed_round_trips() {
        let line = serde_json::to_string(&PanelFrame::InputChanged {
            text: "run book".into(),
        })
        .unwrap();
        assert!(line.contains("\"type\":\"input_changed\""));
        assert!(matches!(
            decode_line::<PanelFrame>(&line),
            Some(PanelFrame::InputChanged { text }) if text == "run book"
        ));
    }

    #[test]
    fn snapshot_redacts_secrets() {
        // OpenAI-style key
        let s = input_snapshot("use sk-abcdefghijklmnop1234 please");
        assert_eq!(s, "use [redacted] please");
        // AWS access key id
        let s = input_snapshot("key AKIAIOSFODNN7EXAMPLE here");
        assert_eq!(s, "key [redacted] here");
        // 32+ char hex run
        let s = input_snapshot("token 0123456789abcdef0123456789abcdef end");
        assert_eq!(s, "token [redacted] end");
        // Normal prose untouched, including 31-char hex (below threshold)
        assert_eq!(input_snapshot("fix the panel"), "fix the panel");
        let hex31 = "0123456789abcdef0123456789abcde";
        assert_eq!(input_snapshot(hex31), hex31);
    }

    #[test]
    fn snapshot_redacts_across_newlines() {
        let s = input_snapshot("line1\nsk-abcdefghijklmnop1234\tend");
        assert_eq!(s, "line1 [redacted] end");
    }

    #[test]
    fn snapshot_truncates_to_tail() {
        let long = "a".repeat(INPUT_SNAPSHOT_MAX_CHARS + 100) + " tail";
        let s = input_snapshot(&long);
        assert!(s.chars().count() <= INPUT_SNAPSHOT_MAX_CHARS);
        assert!(s.ends_with(" tail")); // keep what the user is typing now
    }
}
