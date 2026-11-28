//! Composer chrome — the Enter/newline hints, the input border and padding,
//! and the input widget's construction — moved out of `app/mod.rs` for
//! CLAUDE.md §4's 800-line rule. Pure movement: items are verbatim apart
//! from the visibility needed to re-export them from `app`.

use super::*;

/// Compact composer-border hint when the kitty keyboard protocol is active.
/// Shift+Enter doesn't need an OS-specific label (the key is "Shift"
/// everywhere); the Alt/Option fallback chord only shows up in the full hint
/// below, where there's room to spell it out.
const ENTER_HINT_COMPACT: &str = " message — Enter · Shift+Enter · /help ";

/// Full composer-border hint when the kitty keyboard protocol is active.
/// macOS calls the modifier "Option" even though it's still crossterm's
/// `ALT` — every other OS calls it "Alt".
#[cfg(target_os = "macos")]
const ENTER_HINT_FULL: &str = " message — Enter to send · Shift+Enter newline (Option+Enter also works) · Ctrl+V image · Ctrl+O transcript · /help · Ctrl+D quit";
#[cfg(not(target_os = "macos"))]
const ENTER_HINT_FULL: &str = " message — Enter to send · Shift+Enter newline (Alt+Enter also works) · Ctrl+V image · Ctrl+O transcript · /help · Ctrl+D quit";

/// Hints for a terminal without the kitty protocol (e.g. macOS Terminal.app):
/// there Shift+Enter is indistinguishable from Enter and sends the message,
/// so the hint must not teach it. Alt/Option+Enter works everywhere.
#[cfg(target_os = "macos")]
const ENTER_HINT_COMPACT_FALLBACK: &str = " message — Enter · Option+Enter · /help ";
#[cfg(not(target_os = "macos"))]
const ENTER_HINT_COMPACT_FALLBACK: &str = " message — Enter · Alt+Enter · /help ";
#[cfg(target_os = "macos")]
const ENTER_HINT_FULL_FALLBACK: &str = " message — Enter to send · Option+Enter newline · Ctrl+V image · Ctrl+O transcript · /help · Ctrl+D quit";
#[cfg(not(target_os = "macos"))]
const ENTER_HINT_FULL_FALLBACK: &str = " message — Enter to send · Alt+Enter newline · Ctrl+V image · Ctrl+O transcript · /help · Ctrl+D quit";

/// One-time startup notice for a terminal without the kitty protocol.
#[cfg(target_os = "macos")]
const NEWLINE_FALLBACK_NOTICE: &str =
    "This terminal doesn't support Shift+Enter for newlines — use Option+Enter instead.";
#[cfg(not(target_os = "macos"))]
const NEWLINE_FALLBACK_NOTICE: &str =
    "This terminal doesn't support Shift+Enter for newlines — use Alt+Enter instead.";

/// Composer hint for a given protocol state. Pure (no global read) so tests
/// can cover both states without racing on `keyboard_enhancement_active`.
pub fn enter_hint_for(active: bool, full: bool) -> &'static str {
    match (active, full) {
        (true, true) => ENTER_HINT_FULL,
        (true, false) => ENTER_HINT_COMPACT,
        (false, true) => ENTER_HINT_FULL_FALLBACK,
        (false, false) => ENTER_HINT_COMPACT_FALLBACK,
    }
}

/// Composer hint for the terminal this session is actually running in.
fn enter_hint(full: bool) -> &'static str {
    enter_hint_for(super::super::keyboard_enhancement_active(), full)
}

/// Startup notice for a given protocol state — `None` when Shift+Enter works.
pub fn newline_fallback_notice(active: bool) -> Option<&'static str> {
    (!active).then_some(NEWLINE_FALLBACK_NOTICE)
}

/// Columns a bordered block spends on its own corners, unavailable to a title.
const BORDER_CORNERS: usize = 2;

impl App {
    /// Update the input textarea's border to reflect the current content mode.
    /// Call once per render frame so the style stays in sync without needing
    /// `&mut App` inside the draw closure.
    pub fn sync_input_block(&mut self) {
        let theme = self.theme;
        // The long hint only goes up when it fits. Ratatui clips a Block title
        // that overruns the block, and the clip lands mid-word — the composer
        // border used to end in "… /help · Ct" on anything but a very wide
        // terminal. Two border corners plus the pane padding are unavailable
        // to the title.
        let budget = usize::from(self.width)
            .saturating_sub(usize::from(theme.inner_padding) * 2 + BORDER_CORNERS);
        let full = enter_hint(true);
        let hint = if theme.compact_input || full.chars().count() > budget {
            enter_hint(false)
        } else {
            full
        };
        let is_shell = self.input_text().trim_start().starts_with('!');
        let block = if is_shell {
            Block::default()
                .borders(Borders::TOP)
                .border_type(theme.border_type)
                .border_style(theme.error)
                .padding(composer_padding(theme))
                .title(" ! shell command — output shared with agent ")
        } else {
            Block::default()
                .borders(Borders::TOP)
                .border_type(theme.border_type)
                .border_style(theme.border)
                .padding(composer_padding(theme))
                .title(hint)
                .title_style(theme.muted)
        };
        self.input.set_block(block);
    }
}

/// Build the styled multiline input widget.
/// The composer's inner padding: the skin's horizontal padding, plus one
/// blank row under the text so the input does not sit jammed against the
/// status bar. Nothing above it — the titled rule is its own separation, and
/// a blank there too read as a hole (field report).
fn composer_padding(theme: &Theme) -> Padding {
    let h = u16::from(theme.inner_padding);
    Padding::new(h, h, 0, COMPOSER_PAD_BELOW)
}

/// Blank rows between the composer text and the status bar.
/// `ui::INPUT_H_MIN` counts them; change both together.
pub const COMPOSER_PAD_BELOW: u16 = 1;

/// Composer placeholder when no ghost suggestion is showing.
pub const INPUT_PLACEHOLDER: &str = "Type a message…";

pub(super) fn new_input(theme: &'static Theme) -> TextArea<'static> {
    let mut ta = TextArea::default();
    ta.set_block(
        Block::default()
            .borders(Borders::TOP)
            .padding(Padding::new(0, 0, 0, COMPOSER_PAD_BELOW))
            .title(enter_hint(false)),
    );
    ta.set_cursor_line_style(Style::default());
    ta.set_placeholder_text(INPUT_PLACEHOLDER);
    ta.set_placeholder_style(theme.muted);
    ta
}
