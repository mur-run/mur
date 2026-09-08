//! One message's lines: role header, body, gap before it.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::super::app::{ChatMsg, Role, SPINNER, Severity};
use super::super::markdown;

/// Body indent under a role header ("you ›" / "● agent") so a message's content
/// reads as belonging to its speaker rather than sitting flush with the header.
pub(super) const MSG_INDENT: &str = markdown::BODY_INDENT;

/// Prepend the body indent to an already-styled line (e.g. cached markdown).
pub(super) fn indent_line(mut line: Line<'static>) -> Line<'static> {
    line.spans.insert(0, Span::raw(MSG_INDENT));
    line
}

/// Fixed separator width for flushed scrollback lines: the real frame width
/// isn't known at flush time (content prints above the inline viewport and
/// the terminal soft-wraps it), so a modest fixed rule stands in.
pub(super) const SEPARATOR_WIDTH: usize = 60;

/// Same, but rendered the way the message will look once it SETTLES — used for
/// every flush decision.
///
/// A streaming body paints raw (markdown is only parsed at finish time), and
/// raw is taller: markdown collapses the blank lines between list items and
/// paragraphs. Measuring raw over-commits — the band would be full while
/// streaming and then, the moment the turn settles and re-renders shorter,
/// short by exactly the lines markdown folded away, with no way to pull them
/// back out of scrollback. Measuring settled means the band is exactly full
/// after the turn; while streaming it simply tail-follows, as it always has.
/// Whether a gap row goes before this message.
///
/// A gap means "the speaker changed". A step card is a step *inside* the turn
/// that precedes it, so a run of ten tool calls used to cost ten gap rows and
/// read as ten unrelated events — twenty rows of scrollback for ten facts.
/// Suppressing the gap there is what turns vertical rhythm into grouping
/// instead of uniform repetition: every remaining gap now marks a real
/// boundary.
pub(super) fn wants_gap_before(m: &crate::cmd::agent::cli::app::ChatMsg) -> bool {
    m.step.is_none()
}

/// The gap row itself — a rule when the skin asks for one AND the gap is a
/// change of speaker between two spoken turns; otherwise a blank.
///
/// A System notice is the UI talking, not a speaker: three `/skin` switches
/// used to draw three rules, and `/skills` output followed by its own notice
/// read as four unrelated events. Notices group under a blank, and a rule is
/// left meaning exactly one thing — the conversation moved to the other side.
pub(super) fn gap_row(
    theme: &'static crate::cmd::agent::cli::theme::Theme,
    prev: Option<&crate::cmd::agent::cli::app::ChatMsg>,
    m: &crate::cmd::agent::cli::app::ChatMsg,
) -> Line<'static> {
    let spoken = |r: Role| r != Role::System;
    if theme.show_separator && prev.is_none_or(|p| spoken(p.role)) && spoken(m.role) {
        Line::styled(
            "─".repeat(SEPARATOR_WIDTH),
            Style::default().fg(theme.separator),
        )
    } else {
        Line::default()
    }
}

/// Header line of an agent turn plus its reasoning block: an animated bullet
/// while streaming, a solid one once done — so an in-progress turn reads as
/// "live" at a glance vs. a finished one. Reasoning stays visible after the
/// turn finishes (D5).
pub(super) fn push_agent_header(
    lines: &mut Vec<Line<'static>>,
    m: &ChatMsg,
    spinner: usize,
    theme: &'static crate::cmd::agent::cli::theme::Theme,
) {
    let (bullet, header_style) = if m.streaming {
        let spin = SPINNER[spinner % SPINNER.len()];
        (format!("{spin} agent"), Style::default().fg(theme.agent))
    } else {
        (
            "● agent".to_string(),
            Style::default()
                .fg(theme.agent)
                .add_modifier(Modifier::BOLD),
        )
    };
    lines.push(Line::from(Span::styled(bullet, header_style)));
    if !m.thinking.is_empty() {
        for l in m.thinking.lines() {
            lines.push(Line::styled(
                format!("{MSG_INDENT}{l}"),
                Style::default()
                    .fg(theme.thinking)
                    .add_modifier(Modifier::ITALIC | Modifier::DIM),
            ));
        }
    }
}

/// Body lines of an agent turn — no header, no reasoning: raw text plus a
/// trailing spinner while streaming, markdown-rendered once settled.
///
/// `cached` is the markdown rendered once at finish time for the WHOLE message
/// (`ChatMsg::rendered`); pass `None` when rendering a slice of it, so the
/// slice gets its own render instead of the whole reply's.
pub(super) fn agent_body_lines(
    text: &str,
    streaming: bool,
    spinner: usize,
    theme: &'static crate::cmd::agent::cli::theme::Theme,
    cached: Option<&Vec<Line<'static>>>,
    width: u16,
) -> Vec<Line<'static>> {
    if streaming {
        let mut body: Vec<Line<'static>> = text
            .lines()
            .map(|l| Line::raw(format!("{MSG_INDENT}{l}")))
            .collect();
        // Trailing spinner so the user sees liveness.
        let spin = SPINNER[spinner % SPINNER.len()];
        match body.last_mut() {
            Some(last) => last.spans.push(Span::styled(
                format!(" {spin}"),
                Style::default().fg(theme.agent),
            )),
            None => body.push(Line::styled(
                spin.to_string(),
                Style::default().fg(theme.agent),
            )),
        }
        body
    } else if let Some(cached) = cached {
        // Finished reply: reuse the markdown rendered once at finish time.
        cached.iter().cloned().map(indent_line).collect()
    } else {
        markdown::render(text, markdown::body_cols(width, theme.inner_padding))
            .lines
            .into_iter()
            .map(indent_line)
            .collect()
    }
}

pub(super) fn push_message(
    lines: &mut Vec<Line<'static>>,
    m: &ChatMsg,
    spinner: usize,
    theme: &'static crate::cmd::agent::cli::theme::Theme,
    cards_expanded: bool,
    width: u16,
) {
    // Step cards replace role-based rendering entirely for that message.
    if let Some(card) = &m.step {
        lines.extend(crate::cmd::agent::cli::render_card::card_lines(
            card,
            theme,
            cards_expanded,
            width,
        ));
        return;
    }
    match m.role {
        Role::User => {
            lines.push(Line::from(Span::styled(
                "you ›",
                Style::default().fg(theme.user).add_modifier(Modifier::BOLD),
            )));
            for l in m.text.lines() {
                lines.push(Line::styled(
                    format!("{MSG_INDENT}{l}"),
                    Style::default().fg(theme.user_text),
                ));
            }
        }
        Role::System => {
            // Severity paints the whole note and picks a lead glyph, so a
            // warning reads amber and a success reads green at a glance.
            let (color, glyph) = match m.severity {
                Severity::Info => (theme.system, "·"),
                Severity::Warn => (theme.warn, "▲"),
                Severity::Error => (theme.error, "✖"),
                Severity::Success => (theme.success, "✔"),
            };
            let bold = !matches!(m.severity, Severity::Info);
            for (i, l) in m.text.lines().enumerate() {
                let prefix = if i == 0 {
                    format!("{glyph} ")
                } else {
                    "  ".to_string()
                };
                let mut style = Style::default().fg(color);
                if bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                lines.push(Line::styled(format!("{prefix}{l}"), style));
            }
        }
        Role::Shell => {
            // `$ cmd` highlighted, output dim — visually a local terminal block.
            let mut it = m.text.lines();
            if let Some(first) = it.next() {
                lines.push(Line::styled(
                    first.to_string(),
                    Style::default()
                        .fg(theme.shell)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            for l in it {
                lines.push(Line::styled(
                    l.to_string(),
                    Style::default().fg(theme.system),
                ));
            }
        }
        Role::Agent => {
            // Animated bullet while streaming, solid bullet once done — so an
            // in-progress turn reads as "live" at a glance vs. a finished one.
            push_agent_header(lines, m, spinner, theme);
            lines.extend(agent_body_lines(
                &m.text,
                m.streaming,
                spinner,
                theme,
                m.rendered.as_ref(),
                width,
            ));
            // Drawn here, not baked into `rendered`: this is the only place
            // that knows the pane width, and it runs every frame, so the card
            // reflows on resize for free.
            if let Some(body) = &m.settlement {
                let inner = width.saturating_sub(u16::from(theme.inner_padding) * 2);
                lines.extend(crate::cmd::agent::cli::settlement::card_lines(
                    body, theme, inner,
                ));
            }
        }
    }
}

#[cfg(test)]
mod settlement_paint_tests {
    use super::push_message;
    use crate::cmd::agent::cli::app::{ChatMsg, Role};
    use crate::cmd::agent::cli::theme::DARK;

    #[test]
    fn a_carried_settlement_is_painted_after_the_body() {
        let mut m = ChatMsg::for_test(Role::Agent, "did it");
        m.settlement = Some("  ✔ bash · cargo test".into());
        let mut lines = Vec::new();
        push_message(&mut lines, &m, 0, &DARK, false, 60);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(
            text.iter().any(|l| l.contains("SETTLEMENT")),
            "card missing: {text:?}"
        );
        assert!(
            text.iter().any(|l| l.contains("cargo test")),
            "card body missing: {text:?}"
        );
    }

    #[test]
    fn a_message_without_one_paints_nothing_extra() {
        let m = ChatMsg::for_test(Role::Agent, "did it");
        let mut lines = Vec::new();
        push_message(&mut lines, &m, 0, &DARK, false, 60);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(!text.iter().any(|l| l.contains("SETTLEMENT")), "{text:?}");
    }
}

#[cfg(test)]
mod gap_tests {
    use super::{gap_row, wants_gap_before};
    use crate::cmd::agent::cli::app::{ChatMsg, Role};
    use crate::cmd::agent::cli::step::StepCard;
    use crate::cmd::agent::cli::theme::{DARK, MUR};

    fn card_msg() -> ChatMsg {
        ChatMsg::tool_for_test(StepCard::new(
            "s".into(),
            "bash".into(),
            serde_json::json!({"command": "ls"}),
        ))
    }

    /// The whole point: a run of tool calls is one block of work, not N
    /// separate events. Ten calls used to cost ten gap rows.
    #[test]
    fn a_tool_call_does_not_open_a_gap() {
        assert!(!wants_gap_before(&card_msg()));
    }

    /// Control — if this ever flips, gaps stop marking anything at all.
    #[test]
    fn a_spoken_turn_still_opens_a_gap() {
        assert!(wants_gap_before(&ChatMsg::for_test(Role::User, "hi")));
        assert!(wants_gap_before(&ChatMsg::for_test(Role::Agent, "hello")));
        assert!(wants_gap_before(&ChatMsg::for_test(Role::System, "note")));
    }

    /// One builder for the row, so the two emit paths cannot drift into
    /// different-looking gaps.
    #[test]
    fn the_gap_row_follows_the_skin() {
        let (u, a) = (
            ChatMsg::for_test(Role::User, "hi"),
            ChatMsg::for_test(Role::Agent, "hello"),
        );
        let blank: String = gap_row(&DARK, Some(&u), &a)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            blank.trim().is_empty(),
            "dark skin uses a blank row: {blank:?}"
        );

        let ruled: String = gap_row(&MUR, Some(&u), &a)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        if MUR.show_separator {
            assert!(ruled.contains('─'), "a ruled skin draws a rule: {ruled:?}");
        }
    }
}

#[cfg(test)]
mod block_tests {
    use super::super::band::message_block;
    use crate::cmd::agent::cli::app::{App, ChatMsg, Role};
    use crate::cmd::agent::cli::step::StepCard;

    fn card() -> ChatMsg {
        ChatMsg::tool_for_test(StepCard::new(
            "s".into(),
            "bash".into(),
            serde_json::json!({"command": "ls"}),
        ))
    }

    fn text(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn is_gap(l: &str) -> bool {
        l.trim().is_empty() || l.chars().all(|c| c == '─')
    }

    /// The bug: the viewport and the emit path disagreed. Same message, same
    /// index — the rows must be identical, gap included.
    #[test]
    fn measured_and_rendered_blocks_agree() {
        let app = App::test_fixture();
        let m = ChatMsg::for_test(Role::Agent, "hello");
        let rendered = text(&message_block(&app, 3, &m, 0, false));
        let measured = text(&message_block(&app, 3, &m, 0, true));
        assert_eq!(
            rendered.iter().filter(|l| is_gap(l)).count(),
            measured.iter().filter(|l| is_gap(l)).count(),
            "the row the flush decision counts must be the row that gets drawn"
        );
    }

    /// A tool call is a step inside the turn before it.
    #[test]
    fn a_step_card_opens_no_gap() {
        let app = App::test_fixture();
        let lines = text(&message_block(&app, 3, &card(), 0, false));
        assert!(!lines.first().is_some_and(|l| is_gap(l)), "{lines:?}");
    }

    /// Control — if this flips, gaps stop marking anything.
    #[test]
    fn a_spoken_turn_opens_a_gap() {
        let app = App::test_fixture();
        let m = ChatMsg::for_test(Role::User, "hi");
        let lines = text(&message_block(&app, 3, &m, 0, false));
        assert!(lines.first().is_some_and(|l| is_gap(l)), "{lines:?}");
    }

    /// Nothing opens the transcript with a blank row, and a continuation
    /// resumes a message whose head is already committed above.
    #[test]
    fn the_first_message_and_continuations_open_no_gap() {
        let app = App::test_fixture();
        let m = ChatMsg::for_test(Role::User, "hi");
        let first = text(&message_block(&app, 0, &m, 0, false));
        assert!(!first.first().is_some_and(|l| is_gap(l)), "{first:?}");
        let resumed = text(&message_block(&app, 3, &m, 2, false));
        assert!(!resumed.first().is_some_and(|l| is_gap(l)), "{resumed:?}");
    }
}
