//! The one table a user skin file is read through: every `Theme` field,
//! its class, and — for an optional token under `inherits: none` — the
//! derivation that fills it. The parser and the `/skin` listing both read
//! it, and `whitelist_covers_theme` fails the build's tests if a `Theme`
//! field is added without a row here.

use super::super::Theme;
use ratatui::style::{Modifier, Style};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Must be present under `inherits: none`.
    Required,
    /// Derived from the file's own required tokens when absent.
    Optional,
}

/// One token row. `derive` is only consulted for an `Optional` row under
/// `inherits: none`, and runs in table order after the file's own values are
/// in place — so a derivation may read any row above it.
pub struct TokenRow {
    pub key: &'static str,
    pub class: Class,
    pub field: fn(&mut Theme) -> &mut Style,
    pub derive: fn(&Theme) -> Style,
}

/// The layout keys, by name. Their parsing and defaults live in the loader;
/// they are listed here so the coverage test sees them.
pub const LAYOUT_KEYS: [&str; 3] = ["border_type", "inner_padding", "compact_input"];

fn no_style(_: &Theme) -> Style {
    Style::new()
}

macro_rules! req {
    ($k:ident) => {
        TokenRow {
            key: stringify!($k),
            class: Class::Required,
            field: |t| &mut t.$k,
            derive: no_style,
        }
    };
}

macro_rules! opt {
    ($k:ident, $derive:expr) => {
        TokenRow {
            key: stringify!($k),
            class: Class::Optional,
            field: |t| &mut t.$k,
            derive: $derive,
        }
    };
}

/// The one-channel rule `theme.rs` documents: a diff row carries its change
/// in the tint or in the ink, never both.
fn diff_ink(bg: Style, signal: Style, text: Style) -> Style {
    if bg.bg.is_none() { signal } else { text }
}

pub const TOKENS: &[TokenRow] = &[
    req!(text),
    req!(muted),
    req!(emphasis),
    req!(accent),
    req!(accent_alt),
    req!(ok),
    req!(warn),
    req!(error),
    req!(border),
    req!(badge),
    opt!(surface, no_style),
    opt!(surface_alt, no_style),
    opt!(settlement_surface, no_style),
    opt!(diff_add_bg, no_style),
    opt!(diff_del_bg, no_style),
    opt!(diff_add_mark, |t| t.ok.add_modifier(Modifier::BOLD)),
    opt!(diff_del_mark, |t| t.error.add_modifier(Modifier::BOLD)),
    opt!(diff_add_text, |t| diff_ink(t.diff_add_bg, t.ok, t.text)),
    opt!(diff_del_text, |t| diff_ink(t.diff_del_bg, t.error, t.text)),
    opt!(settlement_text, |t| t.text),
    opt!(settlement_muted, |t| t.muted),
    opt!(settlement_accent, |t| t.accent),
    opt!(settlement_ok, |t| t.ok),
    opt!(settlement_warn, |t| t.warn),
    opt!(settlement_error, |t| t.error),
    opt!(badge_warn, |t| t
        .warn
        .add_modifier(Modifier::REVERSED)
        .add_modifier(Modifier::BOLD)),
    // Not in the spec's table (the token landed after it was written): code
    // is the `warn` hue family on every built-in skin, so it derives from it.
    opt!(code, |t| t.warn),
];

pub fn token(key: &str) -> Option<&'static TokenRow> {
    TOKENS.iter().find(|r| r.key == key)
}

/// Every style token of a resolved theme, for "does it hold any RGB" and
/// similar whole-skin questions.
pub fn styles(theme: &Theme) -> Vec<Style> {
    let mut t = *theme;
    TOKENS.iter().map(|r| *(r.field)(&mut t)).collect()
}
