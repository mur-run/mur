//! Wire protocol + on-disk session records for the murmur Panel (companion
//! window). Shared by the murmur TUI (socket server side) and
//! `mur-gui-core::panel_bridge` (Hub client side). One JSON object per line.
//! Design: docs/superpowers/specs/2026-07-05-murmur-panel-companion-design.md

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const PANEL_PROTO_VERSION: u32 = 1;

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
}
