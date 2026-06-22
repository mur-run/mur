//! Dedicated detachable chat window per agent (M-h9).
//! Opens `index.html#/chat/<agent>` as a frameless transparent window.

#[cfg(target_os = "macos")]
use tauri::window::{Effect, EffectState, EffectsBuilder};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

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

fn urlenc(s: &str) -> String {
    s.chars().map(|c| if c == ' ' { '+' } else { c }).collect()
}

#[tauri::command]
pub fn open_chat_window(agent_name: String, app: AppHandle) -> Result<(), String> {
    let lbl = label(&agent_name);

    // Single-instance guard: bring existing window to front.
    if let Some(win) = app.get_webview_window(&lbl) {
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(());
    }

    let url = format!("index.html#/chat/{}", urlenc(&agent_name));

    let win = WebviewWindowBuilder::new(&app, &lbl, WebviewUrl::App(url.into()))
        .title(&agent_name)
        .inner_size(780.0, 660.0)
        .min_inner_size(560.0, 480.0)
        .resizable(true)
        .decorations(false)
        .transparent(true)
        .shadow(true)
        .visible(false)
        .build()
        .map_err(|e| e.to_string())?;

    // Apply macOS vibrancy — HudWindow gives the dark frosted-glass look.
    #[cfg(target_os = "macos")]
    let _ = win.set_effects(
        EffectsBuilder::new()
            .effect(Effect::HudWindow)
            .state(EffectState::Active)
            .radius(12.0)
            .build(),
    );

    win.show().map_err(|e| e.to_string())?;

    Ok(())
}
