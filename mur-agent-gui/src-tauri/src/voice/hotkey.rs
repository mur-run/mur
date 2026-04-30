//! Global PTT shortcut. Default `Cmd+Shift+'` on macOS,
//! `Ctrl+Shift+'` elsewhere. User-rebindable via Settings → Voice
//! → Hotkey (rebind UI in M1.6.3).
//!
//! Why not Fn: post-2021 Touch ID Macs route Fn through HIToolbox; it
//! cannot be registered via `RegisterEventHotKey`, so `tauri-plugin-
//! global-shortcut` can't see it without the Accessibility API
//! (which would require a TCC prompt).
//!
//! Default-off contract: hotkey is **not** registered at app startup.
//! `voice_enable` (commands.rs) calls `register_ptt`; `voice_disable`
//! calls `unregister_ptt`. On boot, the supervisor re-registers if the
//! persisted `voice_state.json` says the user had voice enabled
//! (mirrors macOS TCC's principle: revocation must outlive process
//! restart, but so must opt-in).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// Stored shortcut config — mirrors a `Shortcut` but in a serialisable
/// form so it can survive an app restart via
/// `<app_data>/voices/hotkey.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyConfig {
    /// Subset of: `super` / `control` / `alt` / `shift`. Multiple may
    /// combine. `super` is Cmd on macOS / Win key on Windows / Super
    /// on Linux.
    pub modifiers: Vec<String>,
    /// `KeyboardEvent.code` string (e.g. `"Quote"`, `"Slash"`,
    /// `"KeyM"`). The frontend rebinder captures the raw code.
    pub code: String,
}

impl HotkeyConfig {
    pub fn default_ptt() -> Self {
        let mods = if cfg!(target_os = "macos") {
            vec!["super".into(), "shift".into()]
        } else {
            vec!["control".into(), "shift".into()]
        };
        Self {
            modifiers: mods,
            code: "Quote".into(),
        }
    }

    pub fn to_shortcut(&self) -> Result<Shortcut> {
        let mut mods = Modifiers::empty();
        for m in &self.modifiers {
            match m.to_lowercase().as_str() {
                "super" | "cmd" | "meta" => mods |= Modifiers::SUPER,
                "control" | "ctrl" => mods |= Modifiers::CONTROL,
                "alt" | "option" => mods |= Modifiers::ALT,
                "shift" => mods |= Modifiers::SHIFT,
                other => anyhow::bail!("unknown modifier `{other}`"),
            }
        }
        let code = parse_code(&self.code)?;
        Ok(Shortcut::new(Some(mods), code))
    }
}

fn parse_code(s: &str) -> Result<Code> {
    Ok(match s {
        "Quote" => Code::Quote,
        "Slash" => Code::Slash,
        "Backslash" => Code::Backslash,
        "Semicolon" => Code::Semicolon,
        "Comma" => Code::Comma,
        "Period" => Code::Period,
        "Minus" => Code::Minus,
        "Equal" => Code::Equal,
        "Space" => Code::Space,
        "Backquote" => Code::Backquote,
        s if s.starts_with("Key") && s.len() == 4 => match s.chars().nth(3) {
            Some('A') => Code::KeyA,
            Some('B') => Code::KeyB,
            Some('C') => Code::KeyC,
            Some('D') => Code::KeyD,
            Some('E') => Code::KeyE,
            Some('F') => Code::KeyF,
            Some('G') => Code::KeyG,
            Some('H') => Code::KeyH,
            Some('I') => Code::KeyI,
            Some('J') => Code::KeyJ,
            Some('K') => Code::KeyK,
            Some('L') => Code::KeyL,
            Some('M') => Code::KeyM,
            Some('N') => Code::KeyN,
            Some('O') => Code::KeyO,
            Some('P') => Code::KeyP,
            Some('Q') => Code::KeyQ,
            Some('R') => Code::KeyR,
            Some('S') => Code::KeyS,
            Some('T') => Code::KeyT,
            Some('U') => Code::KeyU,
            Some('V') => Code::KeyV,
            Some('W') => Code::KeyW,
            Some('X') => Code::KeyX,
            Some('Y') => Code::KeyY,
            Some('Z') => Code::KeyZ,
            _ => anyhow::bail!("unknown key code `{s}`"),
        },
        s if s.starts_with("Digit") && s.len() == 6 => match s.chars().nth(5) {
            Some('0') => Code::Digit0,
            Some('1') => Code::Digit1,
            Some('2') => Code::Digit2,
            Some('3') => Code::Digit3,
            Some('4') => Code::Digit4,
            Some('5') => Code::Digit5,
            Some('6') => Code::Digit6,
            Some('7') => Code::Digit7,
            Some('8') => Code::Digit8,
            Some('9') => Code::Digit9,
            _ => anyhow::bail!("unknown digit code `{s}`"),
        },
        "F1" => Code::F1,
        "F2" => Code::F2,
        "F3" => Code::F3,
        "F4" => Code::F4,
        "F5" => Code::F5,
        "F6" => Code::F6,
        "F7" => Code::F7,
        "F8" => Code::F8,
        "F9" => Code::F9,
        "F10" => Code::F10,
        "F11" => Code::F11,
        "F12" => Code::F12,
        _ => anyhow::bail!("unrecognised key code `{s}`"),
    })
}

pub fn default_ptt_shortcut() -> Shortcut {
    HotkeyConfig::default_ptt()
        .to_shortcut()
        .expect("default ptt shortcut is well-formed")
}

/// Read the persisted hotkey config from
/// `<app_data>/voices/hotkey.json`, falling back to the platform
/// default if the file is missing or corrupt.
pub async fn load_hotkey(app_data_dir: &Path) -> HotkeyConfig {
    let path = app_data_dir.join("voices").join("hotkey.json");
    if !path.exists() {
        return HotkeyConfig::default_ptt();
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice::<HotkeyConfig>(&bytes)
            .unwrap_or_else(|_| HotkeyConfig::default_ptt()),
        Err(_) => HotkeyConfig::default_ptt(),
    }
}

pub async fn save_hotkey(app_data_dir: &Path, cfg: &HotkeyConfig) -> Result<()> {
    let dir = app_data_dir.join("voices");
    tokio::fs::create_dir_all(&dir)
        .await
        .context("create voices/ dir")?;
    let path = dir.join("hotkey.json");
    let bytes = serde_json::to_vec_pretty(cfg).context("serialize hotkey config")?;
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, bytes)
        .await
        .context("write hotkey.json.tmp")?;
    tokio::fs::rename(&tmp, &path)
        .await
        .context("rename hotkey.json")
}

pub fn register_ptt(app: &AppHandle) -> Result<()> {
    register_shortcut(app, &default_ptt_shortcut())
}

/// Register an arbitrary shortcut as the PTT trigger. Used by
/// `register_ptt` (default) and `voice_rebind_hotkey` (custom).
pub fn register_shortcut(app: &AppHandle, shortcut: &Shortcut) -> Result<()> {
    let app_clone = app.clone();
    app.global_shortcut()
        .on_shortcut(*shortcut, move |_app, _shortcut, event| {
            let kind = match event.state() {
                ShortcutState::Pressed => "ptt://hotkey-down",
                ShortcutState::Released => "ptt://hotkey-up",
            };
            let _ = app_clone.emit(kind, ());
        })
        .map_err(|e| anyhow::anyhow!("register PTT shortcut: {e}"))
        .context("register_ptt")
}

/// Unregister the PTT shortcut. Called by `voice_disable` so that
/// disabled-state mur agents don't keep a global hotkey live.
pub fn unregister_ptt(app: &AppHandle) -> Result<()> {
    let shortcut = default_ptt_shortcut();
    app.global_shortcut()
        .unregister(shortcut)
        .map_err(|e| anyhow::anyhow!("unregister PTT shortcut: {e}"))
        .context("unregister_ptt")
}

/// Re-bind PTT to a new shortcut. Best-effort: unregister whatever's
/// currently bound, register the new one, persist on success. Returns
/// the now-effective `HotkeyConfig`.
pub async fn rebind_ptt(app: &AppHandle, cfg: HotkeyConfig) -> Result<HotkeyConfig> {
    let shortcut = cfg.to_shortcut().context("parse new hotkey config")?;
    let _ = app.global_shortcut().unregister_all();
    register_shortcut(app, &shortcut)?;
    let app_data = app
        .path()
        .app_data_dir()
        .context("resolve app_data_dir for hotkey persist")?;
    save_hotkey(&app_data, &cfg).await?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_two_modifiers_and_quote() {
        let c = HotkeyConfig::default_ptt();
        assert_eq!(c.code, "Quote");
        assert_eq!(c.modifiers.len(), 2);
        assert!(c.modifiers.iter().any(|m| m == "shift"));
    }

    #[test]
    fn default_config_round_trips_to_shortcut() {
        let c = HotkeyConfig::default_ptt();
        let s = c.to_shortcut().unwrap();
        let _ = format!("{s:?}");
    }

    #[test]
    fn parse_code_handles_known_strings() {
        assert!(matches!(parse_code("Quote"), Ok(Code::Quote)));
        assert!(matches!(parse_code("KeyM"), Ok(Code::KeyM)));
        assert!(matches!(parse_code("Digit5"), Ok(Code::Digit5)));
        assert!(matches!(parse_code("F2"), Ok(Code::F2)));
    }

    #[test]
    fn parse_code_rejects_unknown_strings() {
        assert!(parse_code("NotAKey").is_err());
        assert!(parse_code("KeyAA").is_err());
    }

    #[test]
    fn unknown_modifier_rejected() {
        let c = HotkeyConfig {
            modifiers: vec!["fn".into()],
            code: "Quote".into(),
        };
        assert!(c.to_shortcut().is_err());
    }

    #[tokio::test]
    async fn load_returns_default_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load_hotkey(dir.path()).await;
        assert_eq!(cfg.code, "Quote");
    }

    #[tokio::test]
    async fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = HotkeyConfig {
            modifiers: vec!["alt".into(), "shift".into()],
            code: "KeyM".into(),
        };
        save_hotkey(dir.path(), &cfg).await.unwrap();
        let loaded = load_hotkey(dir.path()).await;
        assert_eq!(loaded.code, "KeyM");
        assert_eq!(
            loaded.modifiers,
            vec!["alt".to_string(), "shift".to_string()]
        );
    }

    #[tokio::test]
    async fn corrupt_hotkey_file_falls_back_to_default() {
        let dir = tempfile::tempdir().unwrap();
        let voices = dir.path().join("voices");
        std::fs::create_dir_all(&voices).unwrap();
        std::fs::write(voices.join("hotkey.json"), b"not json").unwrap();
        let cfg = load_hotkey(dir.path()).await;
        assert_eq!(cfg.code, "Quote");
    }
}
