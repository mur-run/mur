//! Theme loader. Reads theme dirs from the bundle, validates the
//! schema (including WCAG AA contrast ratios), exposes list /
//! activate.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeDef {
    pub schema_version: u32,
    pub name: String,
    #[serde(default)]
    pub display_name: BTreeMap<String, String>,
    pub kind: String,
    #[serde(default)]
    pub match_system: bool,
    pub colors: BTreeMap<String, String>,
    #[serde(default)]
    pub icons: BTreeMap<String, String>,
}

pub fn list(themes_root: &Path) -> Result<Vec<crate::commands::ThemeInfo>> {
    let mut out = Vec::new();
    for dir in iter_theme_dirs(themes_root)? {
        let theme = load_def(&dir)?;
        let display_name = theme
            .display_name
            .get("default")
            .cloned()
            .unwrap_or_else(|| theme.name.clone());
        out.push(crate::commands::ThemeInfo {
            name: theme.name,
            display_name,
            kind: theme.kind,
        });
    }
    Ok(out)
}

pub fn activate(themes_root: &Path, name: &str) -> Result<ThemeDef> {
    let dir = themes_root.join(name);
    anyhow::ensure!(dir.exists(), "theme '{name}' not found in bundle");
    load_def(&dir)
}

/// Resolve the directory that holds `themes/<name>/theme.json` for
/// the running app. In dev (`cargo tauri dev`) it's next to the
/// source — `CARGO_MANIFEST_DIR/themes`. In a packaged bundle it's
/// under Tauri's resource_dir (e.g.
/// `MyAgent.app/Contents/Resources/themes/`).
///
/// The caller (a Tauri command handler) supplies `resource_dir`
/// from `AppHandle::path().resource_dir()`. If that doesn't exist
/// or doesn't contain a `themes/` child, fall back to the dev path.
pub fn resolve_themes_root(resource_dir: Option<&Path>) -> PathBuf {
    if let Some(rd) = resource_dir {
        let bundled = rd.join("themes");
        if bundled.exists() {
            return bundled;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("themes")
}

fn iter_theme_dirs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(root)
        .with_context(|| format!("read {}", root.display()))?
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() && path.join("theme.json").exists() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn load_def(dir: &Path) -> Result<ThemeDef> {
    let path = dir.join("theme.json");
    let body =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&body).with_context(|| format!("parse {}", path.display()))
}

// ─── WCAG AA contrast validation ──────────────────────────────────

/// Required-pair contrast ratios per spec § 7.7. Returns the list of
/// failing pairs (empty Vec → all pass). Used by the build-time
/// validator (see `validate_all()` below) and by `cargo test`.
#[allow(dead_code)]
pub fn validate_contrast(theme: &ThemeDef) -> Vec<String> {
    let mut failures = Vec::new();
    let pairs: &[(&str, &str, f32, &str)] = &[
        ("fg", "bg", 4.5, "body text"),
        ("fg_secondary", "bg", 4.5, "secondary text"),
        ("accent_fg", "accent", 4.5, "accent button text"),
        ("border", "bg", 3.0, "UI border"),
    ];
    for (fg_key, bg_key, threshold, label) in pairs {
        let Some(fg) = theme.colors.get(*fg_key) else {
            continue;
        };
        let Some(bg) = theme.colors.get(*bg_key) else {
            continue;
        };
        let Some(fg_lum) = relative_luminance(fg) else {
            continue;
        };
        let Some(bg_lum) = relative_luminance(bg) else {
            continue;
        };
        let ratio = contrast_ratio(fg_lum, bg_lum);
        if ratio < *threshold {
            failures.push(format!(
                "{} '{}': {} ({}) vs {} ({}) = {:.2}:1, want ≥ {}:1",
                theme.name, label, fg_key, fg, bg_key, bg, ratio, threshold
            ));
        }
    }
    failures
}

/// Validate every theme on disk; return a single Result. Used as the
/// build-time check in `mur agent export --format gui` (phase 3) +
/// in CI via `cargo test --lib theme::tests::all_builtin_themes`.
#[allow(dead_code)]
pub fn validate_all(themes_root: &Path) -> Result<()> {
    let mut all_failures = Vec::new();
    for dir in iter_theme_dirs(themes_root)? {
        let theme = load_def(&dir)?;
        all_failures.extend(validate_contrast(&theme));
    }
    if !all_failures.is_empty() {
        anyhow::bail!(
            "WCAG AA contrast validation failed:\n  {}",
            all_failures.join("\n  ")
        );
    }
    Ok(())
}

#[allow(dead_code)]
fn relative_luminance(hex: &str) -> Option<f32> {
    let (r, g, b) = parse_hex(hex)?;
    let f = |c: u8| {
        let cs = c as f32 / 255.0;
        if cs <= 0.03928 {
            cs / 12.92
        } else {
            ((cs + 0.055) / 1.055).powf(2.4)
        }
    };
    Some(0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b))
}

#[allow(dead_code)]
fn contrast_ratio(l1: f32, l2: f32) -> f32 {
    let (a, b) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
    (a + 0.05) / (b + 0.05)
}

#[allow(dead_code)]
fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some((r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_on_white_is_max_contrast() {
        let l_black = relative_luminance("#000000").unwrap();
        let l_white = relative_luminance("#ffffff").unwrap();
        let ratio = contrast_ratio(l_black, l_white);
        assert!((ratio - 21.0).abs() < 0.01, "expected 21:1, got {ratio}");
    }

    #[test]
    fn parse_hex_handles_with_and_without_hash() {
        assert_eq!(parse_hex("#0d0221"), Some((0x0d, 0x02, 0x21)));
        assert_eq!(parse_hex("0d0221"), Some((0x0d, 0x02, 0x21)));
        assert_eq!(parse_hex("bad"), None);
    }

    #[test]
    fn all_builtin_themes_pass_wcag_aa() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("themes");
        let failures: Vec<_> = iter_theme_dirs(&root)
            .unwrap()
            .into_iter()
            .map(|d| load_def(&d).unwrap())
            .flat_map(|t| validate_contrast(&t))
            .collect();
        assert!(
            failures.is_empty(),
            "WCAG failures in built-in themes:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn resolve_themes_root_prefers_bundle_when_present() {
        let tmp = std::env::temp_dir().join(format!("mur-themes-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("themes")).unwrap();

        // resource_dir/themes exists → returned as-is.
        let resolved = resolve_themes_root(Some(&tmp));
        assert_eq!(resolved, tmp.join("themes"));

        // resource_dir without themes/ child → falls back to dev path.
        let empty = std::env::temp_dir().join(format!("mur-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&empty);
        std::fs::create_dir_all(&empty).unwrap();
        let fallback = resolve_themes_root(Some(&empty));
        assert!(fallback.ends_with("themes"));
        assert_ne!(fallback, empty.join("themes"));

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&empty);
    }
}
