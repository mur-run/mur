//! Panel window positioning. SINGLE seam for the placement policy:
//! snap-once today, live-follow (AXObserver) later would simply
//! re-invoke [`reposition`] on terminal move/resize.

use tauri::Manager;

use crate::geometry::{Rect, anchor_panel, clamp_into};

pub const PANEL_W: f64 = 360.0;
pub const PANEL_H: f64 = 560.0;
/// Fallback: top-right margin when no terminal window is found.
const FALLBACK_MARGIN: i32 = 16;

/// Map `TERM_PROGRAM` to a CGWindow owner-name needle.
pub fn owner_name_for(term_program: &str) -> &str {
    match term_program {
        "Apple_Terminal" => "Terminal",
        "iTerm.app" => "iTerm2",
        "WezTerm" => "wezterm-gui",
        "ghostty" => "Ghostty",
        "kitty" => "kitty",
        other => other,
    }
}

/// Place the panel beside `target` (a terminal window's bounds, physical px)
/// or at the primary screen's right edge when `None`. Clamped on-screen.
pub fn reposition(win: &tauri::WebviewWindow, target: Option<Rect>) {
    let scale = win.scale_factor().unwrap_or(1.0);
    let size = ((PANEL_W * scale) as i32, (PANEL_H * scale) as i32);
    let pos = match target {
        Some(t) => {
            let mon = crate::pet::monitor_rect_for_point(win.app_handle(), t.x, t.y);
            anchor_panel(t, size, mon)
        }
        None => {
            let Ok(Some(m)) = win.primary_monitor() else {
                return;
            };
            let mon = Rect {
                x: m.position().x,
                y: m.position().y,
                w: m.size().width as i32,
                h: m.size().height as i32,
            };
            clamp_into(
                (
                    mon.right() - size.0 - FALLBACK_MARGIN,
                    mon.y + FALLBACK_MARGIN * 4,
                ),
                size,
                mon,
            )
        }
    };
    let _ = win.set_position(tauri::PhysicalPosition::new(pos.0, pos.1));
}

/// Frontmost window bounds (physical px) of the terminal app named by
/// `TERM_PROGRAM`. No Accessibility / Screen Recording permission needed:
/// CGWindowList exposes bounds + owner without either.
#[cfg(target_os = "macos")]
pub fn terminal_window_bounds(win: &tauri::WebviewWindow, term_program: &str) -> Option<Rect> {
    let (x, y, w, h) = cg::frontmost_window_bounds(owner_name_for(term_program))?;
    // CG reports logical points, top-left origin — same space Tauri's
    // physical px divide into by scale. ponytail: single scale factor from
    // the panel's own window; per-monitor mixed-DPI correctness arrives with
    // live-follow.
    let scale = win.scale_factor().unwrap_or(1.0);
    Some(Rect {
        x: (x * scale) as i32,
        y: (y * scale) as i32,
        w: (w * scale) as i32,
        h: (h * scale) as i32,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn terminal_window_bounds(_win: &tauri::WebviewWindow, _term_program: &str) -> Option<Rect> {
    None
}

#[cfg(target_os = "macos")]
mod cg {
    use core_foundation::array::{CFArray, CFArrayRef};
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFArrayRef;
    }
    const ON_SCREEN_ONLY: u32 = 1 << 0; // kCGWindowListOptionOnScreenOnly
    const EXCLUDE_DESKTOP: u32 = 1 << 4; // kCGWindowListExcludeDesktopElements

    /// First (= frontmost; the list is front-to-back) layer-0 window whose
    /// owner name contains `owner` (case-insensitive). Returns logical
    /// points (x, y, w, h).
    pub fn frontmost_window_bounds(owner: &str) -> Option<(f64, f64, f64, f64)> {
        let raw = unsafe { CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0) };
        if raw.is_null() {
            return None;
        }
        let arr: CFArray<CFDictionary<CFString, CFType>> =
            unsafe { CFArray::wrap_under_create_rule(raw as _) };
        let want = owner.to_lowercase();
        for dict in arr.iter() {
            let owner_name = dict
                .find(CFString::from_static_string("kCGWindowOwnerName"))
                .and_then(|v| v.downcast::<CFString>())
                .map(|s| s.to_string().to_lowercase());
            if !owner_name.is_some_and(|n| n.contains(&want)) {
                continue;
            }
            let layer = dict
                .find(CFString::from_static_string("kCGWindowLayer"))
                .and_then(|v| v.downcast::<CFNumber>())
                .and_then(|n| n.to_i32());
            if layer != Some(0) {
                continue; // menubar/status-item windows
            }
            let bounds = dict
                .find(CFString::from_static_string("kCGWindowBounds"))
                .and_then(|v| v.downcast::<CFDictionary>())?;
            // Raw CFDictionaryGetValue: the typed `find` on the inner bounds
            // dict trips over coexisting core_foundation versions in the tree.
            let num = |k: &'static str| -> Option<f64> {
                let key = CFString::from_static_string(k);
                let v = unsafe {
                    core_foundation::dictionary::CFDictionaryGetValue(
                        bounds.as_concrete_TypeRef(),
                        key.as_CFTypeRef() as _,
                    )
                };
                if v.is_null() {
                    return None;
                }
                let n = unsafe { CFNumber::wrap_under_get_rule(v as _) };
                n.to_f64()
            };
            return Some((num("X")?, num("Y")?, num("Width")?, num("Height")?));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_map_covers_known_terminals() {
        assert_eq!(owner_name_for("Apple_Terminal"), "Terminal");
        assert_eq!(owner_name_for("iTerm.app"), "iTerm2");
        assert_eq!(owner_name_for("WezTerm"), "wezterm-gui");
        // Unknown terminals pass through: the CG substring/CI match still
        // has a chance of finding them.
        assert_eq!(owner_name_for("SomethingElse"), "SomethingElse");
    }
}
