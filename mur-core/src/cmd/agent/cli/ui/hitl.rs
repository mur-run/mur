//! The approval (HITL) modal.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

/// Approval modal size, as a percentage of the viewport.
pub(super) const HITL_PCT_X: u16 = 70;

pub(super) const HITL_PCT_Y: u16 = 50;

/// Rows one PgUp/PgDn moves the approval modal's body, used only until the
/// modal has been drawn once and can report its real height.
///
/// A fixed step is not safe on its own: when the box is short (a narrow pane
/// wraps the input into more rows, leaving as few as two visible) a step of 5
/// jumps clean over the rows in between, and the operator is never shown them.
/// Skipping content in the one modal whose whole job is "read this before it
/// runs" is the worst place to lose a line, so `hitl_scroll_step` clamps the
/// step to what the renderer last had room for.
pub(super) const HITL_SCROLL_PAGE: u16 = 5;

/// Rows to page by, given how many body rows the modal last displayed.
///
/// Never larger than the visible window, so paging cannot step over a row that
/// was never on screen. Keeps one row of overlap for reading continuity, and
/// falls back to [`HITL_SCROLL_PAGE`] before the first draw reports a height.
pub(crate) const fn hitl_scroll_step(visible_rows: u16) -> u16 {
    if visible_rows == 0 {
        HITL_SCROLL_PAGE
    } else if visible_rows > 1 {
        visible_rows - 1
    } else {
        1
    }
}

/// Hard-wrap `s` into rows at most `w` display columns wide.
///
/// Character-based rather than word-based: the payload is pretty-printed JSON
/// and shell, where breaking mid-token is honest and dropping the tail is not.
/// Widths come from `unicode_width` so a CJK argument does not overflow the
/// border.
pub(super) fn wrap_row(s: &str, w: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    if w == 0 || s.is_empty() {
        return vec![s.to_string()];
    }
    let mut rows = Vec::new();
    let mut cur = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > w && !cur.is_empty() {
            rows.push(std::mem::take(&mut cur));
            used = 0;
        }
        cur.push(ch);
        used += cw;
    }
    rows.push(cur);
    rows
}

/// Draw the approval modal. Returns the scroll offset it actually used —
/// `scroll` clamped to the content, so the caller's stored offset cannot run
/// away past the end of a short input — and how many body rows it had room to
/// display, which is what the key handler pages by (see `hitl_scroll_step`).
pub(super) fn render_hitl(
    f: &mut Frame,
    theme: &'static crate::cmd::agent::cli::theme::Theme,
    hitl: &crate::cmd::agent::cli::stream::HitlRequest,
    grant_confirm: Option<char>,
    composer_empty: bool,
    scroll: u16,
) -> (u16, u16) {
    let area = centered_rect(HITL_PCT_X, HITL_PCT_Y, f.area());
    let input = serde_json::to_string_pretty(&hitl.tool_input).unwrap_or_default();
    // Header rows stay pinned: scrolling the body must never carry the tool
    // name off-screen, since "which tool" is half of what is being approved.
    let head = vec![
        Line::from(Span::styled(
            hitl.prompt.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(vec![
            Span::styled("tool: ", Style::default().fg(Color::DarkGray)),
            Span::styled(hitl.tool_name.clone(), Style::default().fg(Color::Yellow)),
        ]),
    ];
    let row_w = area.width.saturating_sub(2) as usize;
    // Wrap every line and keep every line (#939). This modal exists so a human
    // reads the command before it runs; a destructive suffix past a horizontal
    // cut, or past a `.take(12)`, is exactly what must not be silently dropped.
    let mut body: Vec<Line> = Vec::new();
    for l in input.lines() {
        for row in wrap_row(l, row_w) {
            body.push(Line::styled(row, Style::default().fg(Color::DarkGray)));
        }
    }
    // When a session-wide grant is armed, the modal shows ONLY the confirm
    // instruction: the operator is answering "do you really mean the whole
    // session?", and re-printing the full key row there invites a reflex press.
    let keys = if let Some(c) = grant_confirm {
        let what = if c == 'a' {
            format!("`{}` for this session", hitl.tool_name)
        } else {
            "ALL tools for this session".to_string()
        };
        Line::from(vec![
            Span::styled(
                format!("press [{c}] again"),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" to allow {what} — any other key cancels")),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                "[y]",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" approve    "),
            Span::styled(
                "[a]",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" always allow this tool (session)    "),
            Span::styled(
                "[A]",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" allow all tools (session)    "),
            Span::styled(
                "[n]",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" deny / Esc"),
        ])
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.accent)
        .title(" approve tool call ");
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);

    // The key row is the only part of this modal that is never optional, so it
    // gets its own chunk. Previously it was the last entry in one clipped
    // Paragraph: a wrapped JSON input pushed it out of the box and left the
    // operator staring at a blocking gate with no visible way to answer it.
    //
    // While the composer holds text the `composer_empty` guard (#893) makes
    // y/a/A/n type instead of decide. Say so, and dim the row: advertising a
    // live key that is inert is how an operator ends up hitting the 5-minute
    // auto-deny wondering why nothing responds (#939).
    let keys_inert = !composer_empty && grant_confirm.is_none();
    let keys_text = if keys_inert {
        let dimmed = Line::from(
            keys.spans
                .iter()
                .map(|s| {
                    Span::styled(
                        s.content.clone(),
                        s.style.add_modifier(Modifier::DIM).fg(Color::DarkGray),
                    )
                })
                .collect::<Vec<_>>(),
        );
        Text::from(vec![
            dimmed,
            Line::styled(
                "these keys type while the composer has text — Ctrl+U clears it",
                Style::default().fg(Color::Yellow),
            ),
        ])
    } else {
        Text::from(keys)
    };
    let keys_h = Paragraph::new(keys_text.clone())
        .wrap(Wrap { trim: false })
        .line_count(inner.width.max(1)) as u16;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(keys_h.max(1))])
        .split(inner);

    // Body = pinned header + a scrolled window over the wrapped input. When it
    // does not all fit, the notice reports what is HIDDEN — the old message
    // printed the number of rows kept, so widening the pane made the "hidden"
    // count go up (#939).
    let body_h = chunks[0].height as usize;
    let room = body_h.saturating_sub(head.len());
    let mut lines = head;
    let mut shown = room;
    let used_scroll = if body.len() > room && room > 1 {
        let visible = room - 1;
        shown = visible;
        let above = (scroll as usize).min(body.len() - visible);
        let below = body.len() - visible - above;
        lines.extend(body.into_iter().skip(above).take(visible));
        let note = if above == 0 {
            format!("… {below} more lines — PgDn to scroll")
        } else {
            format!("… {above} above · {below} below — PgUp/PgDn")
        };
        lines.push(Line::styled(note, Style::default().fg(Color::DarkGray)));
        above as u16
    } else {
        lines.extend(body);
        0
    };
    f.render_widget(Paragraph::new(Text::from(lines)), chunks[0]);
    f.render_widget(
        Paragraph::new(keys_text).wrap(Wrap { trim: false }),
        chunks[1],
    );
    (used_scroll, shown as u16)
}

pub(super) fn centered_rect(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(v[1])[1]
}

#[cfg(test)]
mod hitl_modal_tests {
    use super::render_hitl;
    use crate::cmd::agent::cli::stream::HitlRequest;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// A tool input big enough that, wrapped, it fills the modal on its own.
    fn fat_request() -> HitlRequest {
        HitlRequest {
            hitl_id: "h1".into(),
            step_id: None,
            tool_name: "bash".into(),
            tool_input: serde_json::json!({
                "command": "x".repeat(400),
                "cwd": "/Volumes/Firecuda4tb/Projects/mur",
            }),
            prompt: "Run `bash`?".into(),
            created_at: std::time::Instant::now(),
        }
    }

    #[test]
    fn the_key_row_survives_an_oversized_input() {
        let mut term = Terminal::new(TestBackend::new(88, 24)).unwrap();
        term.draw(|f| {
            render_hitl(
                f,
                &crate::cmd::agent::cli::theme::ANSI,
                &fat_request(),
                None,
                true,
                0,
            );
        })
        .unwrap();
        let dump = term.backend().to_string();
        assert!(
            dump.contains("approve"),
            "the operator cannot answer a gate whose keys are off-screen:\n{dump}"
        );
        assert!(dump.contains("deny"), "{dump}");
    }

    /// #939 §1: the command body must never be cut horizontally. A marker at
    /// the very end of a long single-line command is the thing a destructive
    /// suffix would occupy, so it is what the test looks for.
    #[test]
    fn a_long_command_is_wrapped_not_truncated() {
        let req = HitlRequest {
            tool_input: serde_json::json!({
                "command": format!("git status {} && rm -rf /tmp/DANGER_MARKER", "-".repeat(120)),
            }),
            ..fat_request()
        };
        let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
        term.draw(|f| {
            render_hitl(f, &crate::cmd::agent::cli::theme::ANSI, &req, None, true, 0);
        })
        .unwrap();
        let dump = term.backend().to_string().replace(['\n', ' '], "");
        assert!(
            dump.contains("DANGER_MARKER"),
            "the tail of the command was dropped — that is the whole defect:\n{}",
            term.backend()
        );
    }

    /// #939 §2: the notice counts HIDDEN rows. The old code printed the number
    /// kept, so a taller box reported MORE hidden. Growing the terminal must
    /// make the number go down, never up.
    #[test]
    fn hidden_line_count_falls_as_the_box_grows() {
        let req = HitlRequest {
            tool_input: serde_json::json!({ "command": "echo hi\n".repeat(400) }),
            ..fat_request()
        };
        let hidden_at = |h: u16| -> usize {
            let mut term = Terminal::new(TestBackend::new(100, h)).unwrap();
            term.draw(|f| {
                render_hitl(f, &crate::cmd::agent::cli::theme::ANSI, &req, None, true, 0);
            })
            .unwrap();
            let dump = term.backend().to_string();
            let tail = dump.split("… ").nth(1).expect("a residue notice");
            tail.split_whitespace()
                .next()
                .and_then(|n| n.parse::<usize>().ok())
                .expect("a numeric hidden count")
        };
        let small = hidden_at(20);
        let large = hidden_at(40);
        assert!(
            large < small,
            "a bigger box hid {large} lines vs {small} in a smaller one — \
             the count is tracking box height, not residual content"
        );
    }

    /// A page must never step over a row the operator was not shown. With a
    /// fixed step of 5 and a box short enough to show 2 body rows, one PgDn
    /// moved from rows 1-2 to rows 6-7 and rows 3-5 were never displayed —
    /// silently, in the modal whose entire purpose is reading before running.
    #[test]
    fn paging_never_steps_over_an_unread_row() {
        for visible in 1u16..=40 {
            let step = super::hitl_scroll_step(visible);
            assert!(
                step <= visible,
                "visible={visible} step={step}: a step wider than the window \
                 skips rows that were never on screen"
            );
            assert!(step >= 1, "visible={visible}: a zero step cannot scroll");
        }
        // Before the first draw there is no measured height to clamp to.
        assert_eq!(super::hitl_scroll_step(0), super::HITL_SCROLL_PAGE);
    }

    /// #939 §1+§3: scrolling reaches content that is off-screen at rest, and an
    /// over-large offset is clamped rather than scrolling into blank space.
    #[test]
    fn paging_reveals_the_tail_and_clamps_at_the_end() {
        let req = HitlRequest {
            tool_input: serde_json::json!({ "command": (0..40).map(|i| format!("step{i}")).collect::<Vec<_>>().join("\n") }),
            ..fat_request()
        };
        let draw = |scroll: u16| {
            let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
            term.draw(|f| {
                render_hitl(
                    f,
                    &crate::cmd::agent::cli::theme::ANSI,
                    &req,
                    None,
                    true,
                    scroll,
                );
            })
            .unwrap();
            term.backend().to_string()
        };
        assert!(
            !draw(0).contains("step39"),
            "sanity: the tail starts hidden"
        );
        assert!(
            draw(200).contains("step39"),
            "an offset past the end must clamp to the last page, not blank the body"
        );
    }

    /// #939 §3: while the composer holds text the decision keys type instead of
    /// deciding, so the modal must say so rather than advertising live keys.
    #[test]
    fn a_nonempty_composer_is_announced_on_the_key_row() {
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        term.draw(|f| {
            render_hitl(
                f,
                &crate::cmd::agent::cli::theme::ANSI,
                &fat_request(),
                None,
                false,
                0,
            );
        })
        .unwrap();
        let dump = term.backend().to_string().replace('\n', " ");
        assert!(
            dump.contains("Ctrl+U"),
            "the operator gets no hint that y/a/A/n are inert:\n{dump}"
        );
    }
}
