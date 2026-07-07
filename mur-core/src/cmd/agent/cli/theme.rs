//! Skin/theme definitions for the agent CLI TUI.

#![allow(dead_code)]

use ratatui::style::Color;
use ratatui::widgets::BorderType;

pub struct Theme {
    // ── labels (bold, identifies speaker) ────────────────────────────────────
    pub user: Color,   // "› you" label
    pub agent: Color,  // "● agent" label
    pub accent: Color, // mascot / brand accent (always color-capable)
    pub shell: Color,  // "$ cmd" label for !command output
    // ── body text ─────────────────────────────────────────────────────────────
    pub user_text: Color,  // continuation lines of a user turn
    pub agent_text: Color, // continuation lines of an agent reply
    pub thinking: Color,   // streaming thinking tokens (italic+dim)
    // ── chrome ────────────────────────────────────────────────────────────────
    pub system: Color,       // system hints, errors, slash-cmd output
    pub warn: Color,         // amber: warnings / degraded notices
    pub error: Color,        // red: failures
    pub success: Color,      // green: completed actions
    pub border: Color,       // transcript + input box borders
    pub border_title: Color, // text inside the border title
    pub separator: Color,    // inter-message separator line
    // ── status bar ────────────────────────────────────────────────────────────
    pub status_bg: Color, // status bar background
    pub badge_fg: Color,  // agent-name badge foreground
    pub badge_bg: Color,  // agent-name badge background
    // ── layout ────────────────────────────────────────────────────────────────
    pub border_type: BorderType, // Plain | Rounded | Double
    pub inner_padding: u8,       // horizontal padding inside panes (0–2)
    pub show_separator: bool,    // true = ─── line; false = blank line
    pub compact_input: bool,     // shorten input box hint text
}

pub const DARK: Theme = Theme {
    user: Color::Green,
    agent: Color::Cyan,
    accent: Color::Cyan,
    shell: Color::Green,
    user_text: Color::Rgb(0xb8, 0xb8, 0xb8),
    agent_text: Color::Rgb(0xea, 0xea, 0xea),
    thinking: Color::Rgb(0x8a, 0x8a, 0x8a),
    system: Color::Rgb(0x8a, 0x8a, 0x8a),
    warn: Color::Rgb(0xe5, 0xa5, 0x3a),
    error: Color::Rgb(0xe0, 0x6c, 0x6c),
    success: Color::Rgb(0x6c, 0xc0, 0x7a),
    border: Color::Rgb(0x55, 0x55, 0x55),
    border_title: Color::Rgb(0x70, 0x70, 0x70),
    separator: Color::Rgb(0x45, 0x45, 0x45),
    status_bg: Color::Reset,
    badge_fg: Color::Black,
    badge_bg: Color::Cyan,
    border_type: BorderType::Plain,
    inner_padding: 1,
    show_separator: false,
    compact_input: false,
};

pub const LIGHT: Theme = Theme {
    user: Color::Rgb(0x16, 0x65, 0x34),
    agent: Color::Rgb(0x0e, 0x6b, 0x8c),
    accent: Color::Rgb(0x0e, 0x6b, 0x8c),
    shell: Color::Rgb(0x16, 0x65, 0x34),
    user_text: Color::Rgb(0x22, 0x22, 0x33),
    agent_text: Color::Rgb(0x22, 0x22, 0x33),
    thinking: Color::Rgb(0x88, 0x88, 0x99),
    system: Color::Rgb(0x77, 0x77, 0x88),
    warn: Color::Rgb(0xb5, 0x74, 0x00),
    error: Color::Rgb(0xc0, 0x30, 0x30),
    success: Color::Rgb(0x1c, 0x7a, 0x3a),
    border: Color::Rgb(0xd0, 0xd0, 0xe0),
    border_title: Color::Rgb(0x99, 0x99, 0x99),
    separator: Color::Rgb(0xd8, 0xd8, 0xe8),
    status_bg: Color::Rgb(0xef, 0xef, 0xf5),
    badge_fg: Color::Rgb(0x0e, 0x6b, 0x8c),
    badge_bg: Color::Rgb(0xe0, 0xf0, 0xf8),
    border_type: BorderType::Rounded,
    inner_padding: 1,
    show_separator: true,
    compact_input: false,
};

pub const MUR: Theme = Theme {
    user: Color::Rgb(0xa7, 0x8b, 0xfa),
    agent: Color::Rgb(0xfb, 0xbf, 0x24),
    accent: Color::Rgb(0xfb, 0xbf, 0x24),
    shell: Color::Rgb(0x88, 0x88, 0xcc),
    user_text: Color::Rgb(0xc8, 0xc8, 0xe8),
    agent_text: Color::Rgb(0xe0, 0xe0, 0xf0),
    thinking: Color::Rgb(0x86, 0x86, 0xc0),
    system: Color::Rgb(0x77, 0x77, 0xaa),
    warn: Color::Rgb(0xf0, 0xc0, 0x60),
    error: Color::Rgb(0xf0, 0x80, 0x90),
    success: Color::Rgb(0x80, 0xd0, 0x90),
    border: Color::Rgb(0x50, 0x50, 0x90),
    border_title: Color::Rgb(0x55, 0x55, 0x99),
    separator: Color::Rgb(0x22, 0x22, 0x44),
    status_bg: Color::Rgb(0x09, 0x09, 0x1a),
    badge_fg: Color::Rgb(0xfb, 0xbf, 0x24),
    badge_bg: Color::Rgb(0x22, 0x1a, 0x06),
    border_type: BorderType::Rounded,
    inner_padding: 1,
    show_separator: true,
    compact_input: true,
};

const KNOWN: [(&str, &Theme); 3] = [("dark", &DARK), ("light", &LIGHT), ("mur", &MUR)];

/// Resolve a skin name to a theme. Falls back to `&DARK` for unknown names.
pub fn resolve_skin(name: &str) -> &'static Theme {
    KNOWN
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, t)| *t)
        .unwrap_or(&DARK)
}

/// Return the canonical name of a theme instance, or "dark" as fallback.
pub fn skin_name(theme: &'static Theme) -> &'static str {
    KNOWN
        .iter()
        .find(|(_, t)| std::ptr::eq(*t, theme))
        .map(|(n, _)| *n)
        .unwrap_or("dark")
}

/// True if `name` is a valid skin name.
pub fn is_known_skin(name: &str) -> bool {
    KNOWN.iter().any(|(n, _)| *n == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_known_skins() {
        assert!(std::ptr::eq(resolve_skin("dark"), &DARK));
        assert!(std::ptr::eq(resolve_skin("light"), &LIGHT));
        assert!(std::ptr::eq(resolve_skin("mur"), &MUR));
    }

    #[test]
    fn resolve_unknown_falls_back_to_dark() {
        assert!(std::ptr::eq(resolve_skin("neon"), &DARK));
        assert!(std::ptr::eq(resolve_skin(""), &DARK));
    }

    #[test]
    fn skin_name_round_trips() {
        assert_eq!(skin_name(&DARK), "dark");
        assert_eq!(skin_name(&LIGHT), "light");
        assert_eq!(skin_name(&MUR), "mur");
    }

    #[test]
    fn is_known_skin_validates_names() {
        assert!(is_known_skin("dark"));
        assert!(is_known_skin("light"));
        assert!(is_known_skin("mur"));
        assert!(!is_known_skin("neon"));
        assert!(!is_known_skin("DARK"));
    }
}
