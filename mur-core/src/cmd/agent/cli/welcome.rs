//! Startup welcome screen: ASCII starling mascot ("Star") + concise identity.
//!
//! Progressive-disclosure design: instead of dumping the full command cheatsheet
//! at startup, we show the mascot, a one-line identity, ONE example, and a single
//! `/help` hint. The full reference stays reachable via the `/help` command.
//!
//! The mascot's eye blinks as a wall-clock *event* (open ~4s, blink ~180ms), so
//! an idle session schedules a redraw only when the next blink is actually due —
//! the event loop bounds its select timeout on [`Blink::next_deadline`]. Color is
//! resolved once at startup into [`MascotMode`]; we render in the theme accent
//! when color is available and go fully static (no color, no animation) when
//! `NO_COLOR` is set, stdout isn't a TTY, or `TERM=dumb`.

use std::path::Path;
use std::time::{Duration, Instant};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::theme::Theme;

// ── Mascot frames ─────────────────────────────────────────────────────────────
// `REST` and `BLINK` are byte-identical except the single eye glyph on
// [`EYE_ROW`] (`o` → `-`), which guarantees zero layout shift across a blink.

/// Eye-open mascot rows (pure 7-bit ASCII, fixed width).
pub const MASCOT_REST: [&str; 5] = [
    r#"   .-"-."#,
    r#"  ( o  )>"#,
    r#"   `\_/`,"#,
    r#"    | |"#,
    r#" ~~~~~~~~~"#,
];

/// Eye-closed mascot rows — identical to [`MASCOT_REST`] but the eye is `-`.
pub const MASCOT_BLINK: [&str; 5] = [
    r#"   .-"-."#,
    r#"  ( -  )>"#,
    r#"   `\_/`,"#,
    r#"    | |"#,
    r#" ~~~~~~~~~"#,
];

/// Row index of the blinking eye (0-based). Test-only invariant anchor: the
/// frames are byte-identical except this row, and the render reads the frames
/// directly (`MASCOT_REST`/`MASCOT_BLINK`), so these consts exist purely to let
/// the layout-shift tests pin the eye position.
#[cfg(test)]
const EYE_ROW: usize = 1;
/// Glyph shown when the eye is open / closed (see [`EYE_ROW`]).
#[cfg(test)]
const EYE_OPEN: char = 'o';
#[cfg(test)]
const EYE_CLOSED: char = '-';

// ── Blink timing ──────────────────────────────────────────────────────────────

/// How long the eye stays open between blinks (ms).
const EYE_OPEN_MS: u64 = 4000;
/// How long a single blink lasts (ms).
const BLINK_MS: u64 = 180;

/// How long the eye stays open between blinks.
const EYE_OPEN_FOR: Duration = Duration::from_millis(EYE_OPEN_MS);
/// How long a single blink lasts.
const BLINK_FOR: Duration = Duration::from_millis(BLINK_MS);
/// Full blink cycle (open + closed): the eye is open for [`EYE_OPEN_FOR`], then
/// closed for [`BLINK_FOR`]. Derived from the two segment durations so the cycle
/// length is never a duplicated literal.
const CYCLE: Duration = EYE_OPEN_FOR.saturating_add(BLINK_FOR);

/// Wall-clock blink driver. Render is a pure function of elapsed time, and the
/// event loop only needs to wake at [`Blink::next_deadline`].
#[derive(Debug, Clone, Copy)]
pub struct Blink {
    start: Instant,
}

impl Default for Blink {
    fn default() -> Self {
        Self::new()
    }
}

impl Blink {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    /// Position within the current cycle for a given instant.
    fn phase(&self, now: Instant) -> Duration {
        let elapsed = now.saturating_duration_since(self.start);
        // `Duration` rem is exact; CYCLE is non-zero.
        Duration::from_nanos((elapsed.as_nanos() % CYCLE.as_nanos()) as u64)
    }

    /// True if the eye is open at `now` (open for [`EYE_OPEN_FOR`], then closed).
    pub fn eye_open(&self, now: Instant) -> bool {
        self.phase(now) < EYE_OPEN_FOR
    }

    /// Instant at which the frame after `now` becomes due — the boundary into
    /// the next open/closed segment. The event loop bounds its select timeout on
    /// this so an idle welcome redraws only when a blink actually changes.
    pub fn next_deadline(&self, now: Instant) -> Instant {
        let phase = self.phase(now);
        let remaining = if phase < EYE_OPEN_FOR {
            EYE_OPEN_FOR - phase
        } else {
            CYCLE - phase
        };
        now + remaining
    }
}

// ── Color mode ────────────────────────────────────────────────────────────────

/// How the mascot is colored, resolved ONCE at startup. There is no fake
/// gradient: either we render flat in the theme accent, or we render plain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MascotMode {
    /// No color and no animation (NO_COLOR / non-TTY / TERM=dumb).
    Off,
    /// Flat color in the theme accent; animation enabled.
    Accent(Color),
}

impl MascotMode {
    /// True if the blink animation should run in this mode.
    pub fn animated(self) -> bool {
        matches!(self, MascotMode::Accent(_))
    }
}

/// Resolve the mascot color/animation mode once at startup.
///
/// Honors the de-facto env conventions: `NO_COLOR` (present at any value, per the
/// no-color.org spec) and `TERM=dumb` suppress color; a non-interactive stdout
/// suppresses both color and animation.
pub fn resolve_mascot_mode(theme: &Theme, is_tty: bool) -> MascotMode {
    if !is_tty || no_color_env() || term_is_dumb() {
        return MascotMode::Off;
    }
    MascotMode::Accent(theme.accent)
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

/// Build the welcome transcript lines: mascot + identity + one example + hint.
///
/// `blink` drives the eye frame; when `mode` is [`MascotMode::Off`] the eye is
/// always open and rendered without color.
pub fn welcome_lines(
    theme: &Theme,
    mode: MascotMode,
    agent: &str,
    cwd: Option<&Path>,
    eye_open: bool,
) -> Vec<Line<'static>> {
    let mascot_style = match mode {
        MascotMode::Accent(c) => Style::default().fg(c),
        MascotMode::Off => Style::default().fg(theme.system),
    };
    // In Off mode the eye is always open (truly static — no animation); otherwise
    // the eye frame follows the wall-clock blink phase passed by the caller.
    let rows = if eye_open || matches!(mode, MascotMode::Off) {
        MASCOT_REST
    } else {
        MASCOT_BLINK
    };

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
        Span::styled(
            agent.to_string(),
            Style::default()
                .fg(theme.agent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  shell ", Style::default().fg(theme.separator)),
        Span::styled(pretty_cwd(cwd), Style::default().fg(theme.system)),
    ]));
    lines.push(Line::default());

    // ONE example to seed the first message.
    lines.push(Line::from(vec![
        Span::styled("Try  ", Style::default().fg(theme.system)),
        Span::styled(
            "\"explain this repo\"",
            Style::default()
                .fg(theme.user_text)
                .add_modifier(Modifier::ITALIC),
        ),
    ]));

    // Single discoverability hint — full reference lives behind /help. Surface
    // Ctrl+V image paste here too (it was previously undiscoverable).
    lines.push(Line::from(vec![
        Span::styled("Type ", Style::default().fg(theme.system)),
        Span::styled(
            "/help",
            Style::default()
                .fg(theme.agent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " for commands · Ctrl+V pastes a screenshot",
            Style::default().fg(theme.system),
        ),
    ]));

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_same_row_count() {
        assert_eq!(MASCOT_REST.len(), MASCOT_BLINK.len());
    }

    #[test]
    fn frames_same_width_per_row() {
        for (rest, blink) in MASCOT_REST.iter().zip(MASCOT_BLINK.iter()) {
            assert_eq!(
                rest.chars().count(),
                blink.chars().count(),
                "row width must match so a blink never shifts layout"
            );
        }
    }

    #[test]
    fn frames_differ_in_exactly_one_eye_char() {
        let mut diffs = 0usize;
        for (ri, (rest, blink)) in MASCOT_REST.iter().zip(MASCOT_BLINK.iter()).enumerate() {
            for (ci, (rc, bc)) in rest.chars().zip(blink.chars()).enumerate() {
                if rc != bc {
                    diffs += 1;
                    assert_eq!(ri, EYE_ROW, "the only differing row is the eye row");
                    assert_eq!(rc, EYE_OPEN);
                    assert_eq!(bc, EYE_CLOSED);
                    // The eye column is wherever the open glyph sits on the eye row.
                    assert_eq!(MASCOT_REST[EYE_ROW].chars().nth(ci), Some(EYE_OPEN));
                }
            }
        }
        assert_eq!(diffs, 1, "rest and blink differ in exactly one char");
    }

    #[test]
    fn eye_open_then_closed_within_cycle() {
        let b = Blink::new();
        let t0 = b.start;
        assert!(b.eye_open(t0), "eye open at cycle start");
        assert!(
            b.eye_open(t0 + EYE_OPEN_FOR - Duration::from_millis(1)),
            "still open just before blink"
        );
        assert!(
            !b.eye_open(t0 + EYE_OPEN_FOR + Duration::from_millis(1)),
            "closed during blink"
        );
        assert!(
            b.eye_open(t0 + CYCLE + Duration::from_millis(1)),
            "open again next cycle"
        );
    }

    #[test]
    fn next_deadline_is_segment_boundary() {
        let b = Blink::new();
        let t0 = b.start;
        // Mid-open: next deadline is when the eye closes.
        let d1 = b.next_deadline(t0 + Duration::from_millis(100));
        assert_eq!(d1, t0 + EYE_OPEN_FOR);
        // Mid-blink: next deadline is the end of the cycle.
        let d2 = b.next_deadline(t0 + EYE_OPEN_FOR + Duration::from_millis(10));
        assert_eq!(d2, t0 + CYCLE);
        // Deadlines are always in the future relative to `now`.
        assert!(b.next_deadline(t0) > t0);
    }

    #[test]
    fn off_mode_disables_animation() {
        assert!(!MascotMode::Off.animated());
        assert!(MascotMode::Accent(Color::Cyan).animated());
    }

    #[test]
    fn resolve_off_when_not_tty() {
        let m = resolve_mascot_mode(&super::super::theme::DARK, false);
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
            &super::super::theme::DARK,
            MascotMode::Off,
            "repomanager",
            Some(std::path::Path::new("/a/b/c")),
            true,
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

    #[test]
    fn off_mode_eye_always_open() {
        // Off mode is truly static: even with eye_open=false, render the REST
        // (open-eye) frame — never the blink frame.
        let lines = welcome_lines(
            &super::super::theme::DARK,
            MascotMode::Off,
            "a",
            None,
            false,
        );
        // Mascot rows start at index 1 (after a leading blank line).
        let eye_line: String = lines[1 + EYE_ROW]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(
            eye_line, MASCOT_REST[EYE_ROW],
            "Off mode shows the open eye"
        );
        assert_ne!(eye_line, MASCOT_BLINK[EYE_ROW]);
    }
}
