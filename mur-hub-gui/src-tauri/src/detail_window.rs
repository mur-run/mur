//! Detail windows (Hub 2.0 Phase 2(b)): an agent's or a fleet's detail page in
//! its own document window, loaded from `index.html#/detail/<kind>/<name>`.
//! Mirrors `chat_window`: one window per target, focus-if-exists.

use serde::Deserialize;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::chat_window::{safe_label_part, urlenc};

const INNER_W: f64 = 960.0;
const INNER_H: f64 = 640.0;
const MIN_W: f64 = 720.0;
const MIN_H: f64 = 520.0;
/// Logical offset from the dashboard's top-left so windows cascade from the Hub.
/// `tauri-plugin-window-state` overrides this with the saved position once a
/// window of this label has been moved (it restores on webview-ready, after
/// this runs).
const CASCADE_OFFSET: f64 = 40.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DetailKind {
    Agent,
    Fleet,
}

impl DetailKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Fleet => "fleet",
        }
    }
}

pub(crate) fn detail_label(kind: DetailKind, name: &str) -> String {
    format!("detail-{}-{}", kind.as_str(), safe_label_part(name))
}

#[tauri::command]
pub fn open_detail_window(
    kind: DetailKind,
    name: String,
    title: String,
    app: AppHandle,
) -> Result<(), String> {
    let lbl = detail_label(kind, &name);

    // Single-instance guard: the user explicitly re-opened it, so focus.
    if let Some(win) = app.get_webview_window(&lbl) {
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(());
    }

    let url = format!("index.html#/detail/{}/{}", kind.as_str(), urlenc(&name));
    let builder = WebviewWindowBuilder::new(&app, &lbl, WebviewUrl::App(url.into()))
        .title(&title)
        .inner_size(INNER_W, INNER_H)
        .min_inner_size(MIN_W, MIN_H)
        .resizable(true)
        .visible(false);
    // The dashboard's chrome: traffic lights inside the page, no title text.
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    let win = builder.build().map_err(|e| e.to_string())?;

    // Positions are physical; CASCADE_OFFSET is logical.
    if let Some(dash) = app.get_webview_window("dashboard")
        && let Ok(pos) = dash.outer_position()
    {
        let scale = dash.scale_factor().unwrap_or(1.0);
        let off = (CASCADE_OFFSET * scale) as i32;
        let _ = win.set_position(tauri::PhysicalPosition::new(pos.x + off, pos.y + off));
    }

    win.show().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_label_has_kind_and_safe_name() {
        assert_eq!(detail_label(DetailKind::Agent, "aura"), "detail-agent-aura");
        assert_eq!(
            detail_label(DetailKind::Fleet, "night ops"),
            "detail-fleet-night-ops"
        );
    }

    #[test]
    fn detail_kind_deserializes_lowercase() {
        assert_eq!(
            serde_json::from_str::<DetailKind>("\"agent\"").unwrap(),
            DetailKind::Agent
        );
        assert_eq!(
            serde_json::from_str::<DetailKind>("\"fleet\"").unwrap(),
            DetailKind::Fleet
        );
        assert!(serde_json::from_str::<DetailKind>("\"skill\"").is_err());
    }
}
