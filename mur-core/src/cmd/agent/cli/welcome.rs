//! Startup welcome: ASCII starling mascot ("Star") + concise identity.
//!
//! Progressive-disclosure design: instead of dumping the full command cheatsheet
//! at startup, we show the mascot, a one-line identity, ONE example, and a single
//! `/help` hint. The full reference stays reachable via the `/help` command.
//!
//! The welcome is *printed* — plain terminal output at the cursor, like any
//! command's banner — not painted inside the live viewport. The viewport is a
//! fixed height anchored at the bottom of the window, so the mascot sits at
//! the top, the composer on the floor, and the first message changes neither:
//! the transcript grows upward from the composer and the banner scrolls away
//! only when the conversation is longer than the window. (Painting it in the
//! viewport meant a full-window viewport that had to shrink on the first
//! message, and the re-anchor that shrink needs dropped the whole screen to
//! the floor of a tall terminal.) Color is resolved once at startup into
//! [`MascotMode`]: the theme accent when color is available, plain when
//! `NO_COLOR` is set, stdout isn't a TTY, or `TERM=dumb`.

use std::io::{self, Write};
use std::path::Path;

use crossterm::style::{
    Attribute, Color as CColor, Print, ResetColor, SetAttribute, SetForegroundColor,
};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::theme::Theme;

/// Mascot rows (pure 7-bit ASCII, fixed width).
pub const MASCOT_REST: [&str; 5] = [
    r#"   .-"-."#,
    r#"  ( o  )>"#,
    r#"   `\_/`,"#,
    r#"    | |"#,
    r#" ~~~~~~~~~"#,
];

// ── Color mode ────────────────────────────────────────────────────────────────

/// How the mascot is colored, resolved ONCE at startup. There is no fake
/// gradient: either we render flat in the theme accent, or we render plain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MascotMode {
    /// No color (NO_COLOR / non-TTY / TERM=dumb).
    Off,
    /// Flat color in the theme accent.
    Accent(Color),
}

/// Resolve the mascot color mode once at startup.
///
/// Honors the de-facto env conventions: `NO_COLOR` (present at any value, per the
/// no-color.org spec) and `TERM=dumb` suppress color, as does a non-interactive
/// stdout.
pub fn resolve_mascot_mode(theme: &Theme, is_tty: bool) -> MascotMode {
    if !is_tty || no_color_env() || term_is_dumb() {
        return MascotMode::Off;
    }
    MascotMode::Accent(theme.accent.fg.unwrap_or(Color::Reset))
}

fn no_color_env() -> bool {
    std::env::var_os("NO_COLOR").is_some()
}

fn term_is_dumb() -> bool {
    std::env::var_os("TERM").is_some_and(|v| v == "dumb")
}

// ── Render ────────────────────────────────────────────────────────────────────

/// Max trailing path components shown on the identity line; deeper paths are
/// truncated with a leading `…/` so a long cwd can't overflow the line.
const MAX_CWD_COMPONENTS: usize = 3;

/// Shorten an absolute path for the identity line: home-relative with `~`, then
/// keep only the last [`MAX_CWD_COMPONENTS`] components (prefixed with `…/` when
/// truncated) so a deep path can't blow the line width.
fn pretty_cwd(cwd: Option<&Path>) -> String {
    let Some(cwd) = cwd else {
        return "·".to_string();
    };
    let full = match dirs_home() {
        Some(home) => match cwd.strip_prefix(&home) {
            Ok(rel) if rel.as_os_str().is_empty() => return "~".to_string(),
            Ok(rel) => format!("~/{}", rel.display()),
            Err(_) => cwd.display().to_string(),
        },
        None => cwd.display().to_string(),
    };
    let parts: Vec<&str> = full.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() > MAX_CWD_COMPONENTS {
        format!("…/{}", parts[parts.len() - MAX_CWD_COMPONENTS..].join("/"))
    } else {
        full
    }
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

/// Print `lines` as plain terminal output at the cursor, styled with the
/// theme's colours, one row per line. Raw mode is on by the time this runs,
/// so rows end in `\r\n`.
pub fn print_banner(out: &mut impl Write, lines: &[Line<'static>]) -> io::Result<()> {
    for line in lines {
        for span in &line.spans {
            if let Some(fg) = span.style.fg {
                crossterm::queue!(out, SetForegroundColor(CColor::from(fg)))?;
            }
            let m = span.style.add_modifier;
            if m.contains(Modifier::BOLD) {
                crossterm::queue!(out, SetAttribute(Attribute::Bold))?;
            }
            if m.contains(Modifier::ITALIC) {
                crossterm::queue!(out, SetAttribute(Attribute::Italic))?;
            }
            if m.contains(Modifier::DIM) {
                crossterm::queue!(out, SetAttribute(Attribute::Dim))?;
            }
            crossterm::queue!(
                out,
                Print(span.content.as_ref()),
                SetAttribute(Attribute::Reset),
                ResetColor
            )?;
        }
        crossterm::queue!(out, Print("\r\n"))?;
    }
    out.flush()
}

/// Build the welcome lines: mascot + identity + one example + hint. When
/// `mode` is [`MascotMode::Off`] the mascot is rendered in the muted style.
pub fn welcome_lines(
    theme: &Theme,
    mode: MascotMode,
    agent: &str,
    cwd: Option<&Path>,
) -> Vec<Line<'static>> {
    let mascot_style = match mode {
        MascotMode::Accent(c) => Style::default().fg(c),
        MascotMode::Off => theme.muted,
    };
    let rows = MASCOT_REST;

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(rows.len() + 6);
    lines.push(Line::default());
    for row in rows {
        lines.push(Line::styled(row.to_string(), mascot_style));
    }
    lines.push(Line::default());

    // One-line identity: agent · shell cwd.
    //
    // Labelled "shell:" because that is all it is. This is the directory the
    // TUI was launched from, handed to the agent as a bash-cwd hint on the
    // first message — it is NOT the runtime's session cwd, which stays at
    // `~/.mur/agents/<name>` and is what a relative path in `read_file`
    // resolves against. Unlabelled it read as "the agent works here", so
    // relative paths were handed over that could not resolve and the failure
    // looked like a missing file (#940).
    lines.push(Line::from(vec![
        Span::styled(agent.to_string(), theme.accent.add_modifier(Modifier::BOLD)),
        Span::styled("  ·  shell ", theme.muted),
        Span::styled(pretty_cwd(cwd), theme.muted),
    ]));
    lines.push(Line::default());

    // ONE example to seed the first message.
    lines.push(Line::from(vec![
        Span::styled("Try  ", theme.muted),
        Span::styled(
            "\"explain this repo\"",
            theme
                .text
                .add_modifier(Modifier::DIM)
                .add_modifier(Modifier::ITALIC),
        ),
    ]));

    // Single discoverability hint — full reference lives behind /help. Surface
    // Ctrl+V image paste here too (it was previously undiscoverable).
    lines.push(Line::from(vec![
        Span::styled("Type ", theme.muted),
        Span::styled("/help", theme.accent.add_modifier(Modifier::BOLD)),
        Span::styled(" for commands · Ctrl+V pastes a screenshot", theme.muted),
    ]));

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_off_when_not_tty() {
        let m = resolve_mascot_mode(&super::super::theme::ANSI, false);
        assert_eq!(m, MascotMode::Off);
    }

    #[test]
    fn pretty_cwd_truncates_deep_path() {
        // A deep path keeps only the last MAX_CWD_COMPONENTS components.
        assert_eq!(
            pretty_cwd(Some(std::path::Path::new("/a/b/c/d/e/f"))),
            "…/d/e/f"
        );
        // A shallow path (not under HOME in the test env) is shown as-is.
        assert_eq!(pretty_cwd(Some(std::path::Path::new("/a/b"))), "/a/b");
        // None renders a neutral placeholder.
        assert_eq!(pretty_cwd(None), "·");
    }

    /// #940: the identity line printed a bare path that reads as "the agent
    /// works here". It is the *shell's* cwd, passed along only as a bash hint;
    /// the runtime's session cwd stays at `~/.mur/agents/<name>`, so a relative
    /// path handed to `read_file` on the strength of this line cannot resolve.
    #[test]
    fn the_identity_line_labels_the_path_as_the_shell_cwd() {
        let lines = welcome_lines(
            &super::super::theme::ANSI,
            MascotMode::Off,
            "repomanager",
            Some(std::path::Path::new("/a/b/c")),
        );
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("repomanager"), "agent name stays: {text}");
        assert!(text.contains("/a/b/c"), "path stays: {text}");
        assert!(
            text.contains("shell"),
            "the path must be labelled, not left to read as the agent's cwd: {text}"
        );
    }

    /// The banner is terminal output, not a viewport paint: every row ends in
    /// CRLF (raw mode), the mascot is there, and the accent colour is carried
    /// as an SGR sequence rather than dropped.
    #[test]
    fn the_banner_prints_one_crlf_row_per_line_with_colour() {
        let lines = welcome_lines(
            &super::super::theme::MUR,
            MascotMode::Accent(Color::Rgb(0xfb, 0xbf, 0x24)),
            "mur",
            None,
        );
        let mut out: Vec<u8> = Vec::new();
        print_banner(&mut out, &lines).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text.matches("\r\n").count(),
            lines.len(),
            "one row per line"
        );
        assert!(text.contains(MASCOT_REST[1]), "mascot missing:\n{text}");
        assert!(
            text.contains("\x1b[38;2;251;191;36m"),
            "accent colour dropped:\n{text}"
        );
    }
}
