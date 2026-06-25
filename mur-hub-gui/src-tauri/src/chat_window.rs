//! Dedicated detachable chat window per agent (M-h9).
//! Opens `index.html#/chat/<agent>` as a frameless transparent window.

#[cfg(target_os = "macos")]
use tauri::window::{Effect, EffectState, EffectsBuilder};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// Compact ("panel") default size; the user can expand to full via the header.
const PANEL_W: i32 = 380;
const PANEL_H: i32 = 520;

fn label(agent_name: &str) -> String {
    let safe: String = agent_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("chat-{}", safe)
}

fn pet_label(agent_name: &str) -> String {
    // Same safe-name transform as `label`, with the pet prefix.
    let safe: String = agent_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("pet-{}", safe)
}

fn urlenc(s: &str) -> String {
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
    if let Some(pet) = app.get_webview_window(&pet_label(&agent_name))
        && let (Ok(pp), Ok(ps)) = (pet.outer_position(), pet.outer_size())
    {
        let pet_rect = crate::geometry::Rect {
            x: pp.x,
            y: pp.y,
            w: ps.width as i32,
            h: ps.height as i32,
        };
        let mon = crate::pet::monitor_rect_for_point(&app, pp.x, pp.y);
        let (x, y) = crate::geometry::anchor_panel(pet_rect, (PANEL_W, PANEL_H), mon);
        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    }

    win.show().map_err(|e| e.to_string())?;
    Ok(())
}
