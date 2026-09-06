//! Dedicated detachable chat window per agent (M-h9).
//! Opens `index.html#/chat/<agent>` as a frameless transparent window.

#[cfg(target_os = "macos")]
use tauri::window::{Effect, EffectState, EffectsBuilder};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// Compact ("panel") default size; the user can expand to full via the header.
const PANEL_W: i32 = 380;
const PANEL_H: i32 = 520;

/// The label-safe form of an agent / fleet name: alphanumerics and `-` kept,
/// everything else becomes `-`. Shared by the chat, pet, and detail windows.
pub(crate) fn safe_label_part(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn label(agent_name: &str) -> String {
    format!("chat-{}", safe_label_part(agent_name))
}

fn pet_label(agent_name: &str) -> String {
    format!("pet-{}", safe_label_part(agent_name))
}

/// Spaces become `+` for the hash route; the UI reverses it (`agentNameFromHash`).
pub(crate) fn urlenc(s: &str) -> String {
    s.chars().map(|c| if c == ' ' { '+' } else { c }).collect()
}

#[tauri::command]
pub fn open_chat_window(agent_name: String, app: AppHandle) -> Result<(), String> {
    let lbl = label(&agent_name);

    // Single-instance guard: just show (do NOT set_focus — that steals focus
    // from the user's foreground app and previously raised the Hub too).
    if let Some(win) = app.get_webview_window(&lbl) {
        let _ = win.show();
        let _ = win.set_focus(); // existing window: user explicitly re-opened it
        return Ok(());
    }

    let url = format!("index.html#/chat/{}", urlenc(&agent_name));

    let win = WebviewWindowBuilder::new(&app, &lbl, WebviewUrl::App(url.into()))
        .title(&agent_name)
        .inner_size(PANEL_W as f64, PANEL_H as f64)
        .min_inner_size(320.0, 420.0)
        .resizable(true)
        .decorations(false)
        .transparent(true)
        .shadow(true)
        .always_on_top(true) // sit above the always-on-top pet
        .visible(false)
        .build()
        .map_err(|e| e.to_string())?;

    #[cfg(target_os = "macos")]
    let _ = win.set_effects(
        EffectsBuilder::new()
            .effect(Effect::HudWindow)
            .state(EffectState::Active)
            .radius(12.0)
            .build(),
    );

    // Anchor next to the pet (if it exists) on the pet's monitor.
    // pet.outer_position/outer_size are PHYSICAL; PANEL_W/H are LOGICAL.
    // Convert with the pet window's scale factor before calling anchor_panel.
    if let Some(pet) = app.get_webview_window(&pet_label(&agent_name))
        && let (Ok(pp), Ok(ps)) = (pet.outer_position(), pet.outer_size())
    {
        let scale = pet.scale_factor().unwrap_or(1.0);
        let pet_rect = crate::geometry::Rect {
            x: pp.x,
            y: pp.y,
            w: ps.width as i32,
            h: ps.height as i32,
        };
        let mon = crate::pet::monitor_rect_for_point(&app, pp.x, pp.y);
        let panel_w = (PANEL_W as f64 * scale) as i32;
        let panel_h = (PANEL_H as f64 * scale) as i32;
        let (x, y) = crate::geometry::anchor_panel(pet_rect, (panel_w, panel_h), mon);
        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    }

    win.show().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_label_part_keeps_alphanumerics_and_dashes() {
        assert_eq!(safe_label_part("aura-2"), "aura-2");
        assert_eq!(safe_label_part("My Agent.v2"), "My-Agent-v2");
        assert_eq!(safe_label_part("研究員"), "研究員");
    }

    #[test]
    fn label_prefixes_kind() {
        assert_eq!(label("a b"), "chat-a-b");
        assert_eq!(pet_label("a b"), "pet-a-b");
    }

    #[test]
    fn urlenc_only_touches_spaces() {
        assert_eq!(urlenc("a b-c"), "a+b-c");
    }
}
