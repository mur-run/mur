//! One message's lines: role header, body, gap before it.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::super::app::{ChatMsg, Role, SPINNER, Severity};
use super::super::markdown;

/// Body indent under a role header ("you ›" / "● agent") so a message's content
/// reads as belonging to its speaker rather than sitting flush with the header.
pub(super) const MSG_INDENT: &str = markdown::BODY_INDENT;

/// Prepend the body indent to an already-styled line (e.g. cached markdown).
pub(super) fn indent_line(mut line: Line<'static>) -> Line<'static> {
    // A blank stays a blank. Indenting it makes a whitespace-only line, and
    // ratatui's `Wrap { trim: false }` paints one of those as TWO rows — every
    // paragraph break in a reply showed up double-spaced.
    if line.width() == 0 {
        return line;
    }
    line.spans.insert(0, Span::raw(MSG_INDENT));
    line
}

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
/// The gap before a message: a blank line in every skin. The role label is
/// the change-of-speaker signal; a rule under it repeated the information
/// (spec decision 4). Kept as a function because `message_block` attributes
/// the gap to the message it precedes, and that is what keeps the measured
/// band and the painted band the same rows.
/// Verbs that introduce an approval receipt, whose next token is a tool badge.
const APPROVAL_VERBS: [&str; 2] = ["approved ", "denied "];

/// Separator `approval_summary` puts between the tool badge and the command.
const BADGE_SEP: &str = " · ";

/// Lead-in of the trailing intent note `approval_summary` may append. Shared
/// with `call_summary`, which writes it.
use crate::cmd::agent::cli::call_summary::INTENT_LEAD;

/// Split `approved bash · git push …  (publish branch)` into verb / badge /
/// command / intent so the badge and the intent can be dimmed independently of
/// the command. `None` for any other system note, which keeps ordinary text a
/// single span.
fn split_approval_receipt(line: &str) -> Option<(String, String, String, String)> {
    let verb = APPROVAL_VERBS.iter().find(|v| line.starts_with(**v))?;
    let rest = &line[verb.len()..];
    let sep = rest.find(BADGE_SEP)?;
    // A badge is one bare tool name; a space inside means this is prose that
    // merely happens to start with the verb.
    let badge = &rest[..sep];
    if badge.is_empty() || badge.contains(' ') {
        return None;
    }
    let tail = &rest[sep..];
    // The intent is the LAST parenthetical on the row, and only when it closes
    // the row — a `(` inside the command itself must stay part of the command.
    let (cmd, intent) = match tail.rfind(INTENT_LEAD) {
        Some(i) if tail.ends_with(')') => (&tail[..i], &tail[i..]),
        _ => (tail, ""),
    };
    Some((
        (*verb).to_string(),
        badge.to_string(),
        cmd.to_string(),
        intent.to_string(),
    ))
}

pub(super) fn gap_row(
    _theme: &'static crate::cmd::agent::cli::theme::Theme,
    _prev: Option<&crate::cmd::agent::cli::app::ChatMsg>,
    _m: &crate::cmd::agent::cli::app::ChatMsg,
) -> Line<'static> {
    Line::default()
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
        (format!("{spin} agent"), theme.accent)
    } else {
        (
            "● agent".to_string(),
            theme.accent.add_modifier(Modifier::BOLD),
        )
    };
    lines.push(Line::from(Span::styled(bullet, header_style)));
    if !m.thinking.is_empty() {
        for l in m.thinking.lines() {
            lines.push(Line::styled(
                format!("{MSG_INDENT}{l}"),
                theme.muted.add_modifier(Modifier::ITALIC | Modifier::DIM),
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
            .map(|l| {
                if l.is_empty() {
                    Line::default()
                } else {
                    Line::raw(format!("{MSG_INDENT}{l}"))
                }
            })
            .collect();
        // Trailing spinner so the user sees liveness.
        let spin = SPINNER[spinner % SPINNER.len()];
        match body.last_mut() {
            Some(last) => last
                .spans
                .push(Span::styled(format!(" {spin}"), theme.accent)),
            None => body.push(Line::styled(spin.to_string(), theme.accent)),
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
                theme.accent_alt.add_modifier(Modifier::BOLD),
            )));
            for l in m.text.lines() {
                lines.push(Line::styled(
                    format!("{MSG_INDENT}{l}"),
                    theme.text.add_modifier(Modifier::DIM),
                ));
            }
        }
        Role::System => {
            // Severity paints the whole note and picks a lead glyph, so a
            // warning reads amber and a success reads green at a glance.
            let (style, glyph) = match m.severity {
                Severity::Info => (theme.muted, "·"),
                Severity::Warn => (theme.warn, "▲"),
                Severity::Error => (theme.error, "✖"),
                Severity::Success => (theme.ok, "✔"),
            };
            let bold = !matches!(m.severity, Severity::Info);
            for (i, l) in m.text.lines().enumerate() {
                let prefix = if i == 0 {
                    format!("{glyph} ")
                } else {
                    "  ".to_string()
                };
                let mut style = style;
                if bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                // An approval receipt gets its tool badge and its trailing
                // intent dimmed so the COMMAND is the brightest thing on the
                // row — it is what was actually approved, and the intent is
                // model-written prose that may not match it.
                if i == 0
                    && let Some((verb, badge, cmd, intent)) = split_approval_receipt(l)
                {
                    lines.push(Line::from(vec![
                        Span::styled(format!("{prefix}{verb}"), style),
                        Span::styled(badge, style.add_modifier(Modifier::DIM)),
                        Span::styled(cmd, style),
                        Span::styled(intent, style.add_modifier(Modifier::DIM)),
                    ]));
                    continue;
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
                    theme.accent_alt.add_modifier(Modifier::BOLD),
                ));
            }
            for l in it {
                lines.push(Line::styled(l.to_string(), theme.muted));
            }
            // Live command: the same spinner frame the agent header uses, so
            // the two animate together, plus the one key that ends it (D3).
            // The event loop ticks this for a shell-only command too (§3.6).
            if m.streaming {
                let spin = SPINNER[spinner % SPINNER.len()];
                lines.push(Line::styled(
                    format!("{spin} running · Ctrl-C to stop"),
                    theme.muted,
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
                // A blank row between the reply and its settlement card: the
                // card is a surface of its own and read as part of the last
                // paragraph when it sat flush against it.
                lines.push(Line::default());
                let inner = width.saturating_sub(u16::from(theme.inner_padding) * 2);
                // A settlement is an execution summary, not a full-width alert.
                // Cap it so wide terminals leave it visually subordinate to prose.
                let card_width = inner.min(72);
                lines.extend(crate::cmd::agent::cli::settlement::card_lines(
                    body, theme, card_width,
                ));
            }
        }
    }
}

#[cfg(test)]
mod settlement_paint_tests {
    use super::push_message;
    use crate::cmd::agent::cli::app::{ChatMsg, Role};
    use crate::cmd::agent::cli::theme::ANSI;

    #[test]
    fn a_carried_settlement_is_painted_after_the_body() {
        let mut m = ChatMsg::for_test(Role::Agent, "did it");
        m.settlement = Some("  ✔ bash · cargo test".into());
        let mut lines = Vec::new();
        push_message(&mut lines, &m, 0, &ANSI, false, 60);
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
        let head = text.iter().position(|l| l.contains("SETTLEMENT")).unwrap();
        assert!(
            head > 0 && text[head - 1].trim().is_empty(),
            "no blank row between the reply and its card: {text:?}"
        );
    }

    #[test]
    fn a_message_without_one_paints_nothing_extra() {
        let m = ChatMsg::for_test(Role::Agent, "did it");
        let mut lines = Vec::new();
        push_message(&mut lines, &m, 0, &ANSI, false, 60);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(!text.iter().any(|l| l.contains("SETTLEMENT")), "{text:?}");
    }
}

#[cfg(test)]
mod gap_tests {
    use super::gap_row;
    use crate::cmd::agent::cli::app::{ChatMsg, Role};
    use crate::cmd::agent::cli::theme::{ANSI, MUR};

    /// One builder for the row, so the two emit paths cannot drift into
    /// different-looking gaps.
    #[test]
    fn the_gap_row_is_blank_under_every_skin() {
        let (u, a) = (
            ChatMsg::for_test(Role::User, "hi"),
            ChatMsg::for_test(Role::Agent, "hello"),
        );
        for theme in [&ANSI, &MUR] {
            let row: String = gap_row(theme, Some(&u), &a)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            assert!(row.trim().is_empty(), "a rule between turns: {row:?}");
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

    /// Every item is set off from the one before it, tool cards included: a
    /// run of cards used to group flush against each other and read as one
    /// smear (field report). Each card is its own line of the story.
    #[test]
    fn every_step_card_opens_a_gap() {
        let mut app = App::test_fixture();
        app.messages.push(ChatMsg::for_test(Role::User, "hi"));
        app.messages.push(card());
        app.messages.push(card());
        for i in [1, 2] {
            let lines = text(&message_block(&app, i, &app.messages[i], 0, false));
            assert!(
                lines.first().is_some_and(|l| is_gap(l)),
                "card {i}: {lines:?}"
            );
        }
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

#[cfg(test)]
mod shell_footer_tests {
    use super::push_message;
    use crate::cmd::agent::cli::app::{ChatMsg, Role};
    use crate::cmd::agent::cli::theme::ANSI;

    fn rendered(text: &str, streaming: bool) -> Vec<String> {
        let mut m = ChatMsg::for_test(Role::Shell, text);
        m.streaming = streaming;
        let mut lines = Vec::new();
        push_message(&mut lines, &m, 0, &ANSI, false, 60);
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// A live card says it is running and names the key that ends it (D3);
    /// a finished one says neither, and both keep the `$ cmd` line.
    #[test]
    fn a_running_shell_card_shows_the_footer() {
        let live = rendered("$ cargo test\nCompiling", true);
        assert!(
            live.iter().any(|l| l.contains("running · Ctrl-C to stop")),
            "{live:?}"
        );
        assert!(live.iter().any(|l| l.contains("$ cargo test")), "{live:?}");

        let done = rendered("$ cargo test\nCompiling\n[exit 0]", false);
        assert!(
            !done.iter().any(|l| l.contains("Ctrl-C to stop")),
            "{done:?}"
        );
        assert!(done.iter().any(|l| l.contains("$ cargo test")), "{done:?}");
    }
}

#[cfg(test)]
mod approval_badge_tests {
    use super::push_message;
    use crate::cmd::agent::cli::app::{ChatMsg, Role, Severity};
    use crate::cmd::agent::cli::theme::ANSI;
    use ratatui::style::Modifier;

    /// An approval receipt is three parts, not one flat string: the verb, a
    /// DIM tool badge, and the command carrying the severity colour. The
    /// command is the approval target, so it must not be the dimmest thing on
    /// the row.
    #[test]
    fn an_approval_receipt_dims_the_badge_not_the_command() {
        let mut m = ChatMsg::for_test(Role::System, "approved bash · git push -u origin feat/x");
        m.severity = Severity::Success;
        let mut lines = Vec::new();
        push_message(&mut lines, &m, 0, &ANSI, false, 80);
        let row = lines.first().expect("one row");
        assert!(row.spans.len() >= 3, "badge must be its own span: {row:?}");
        let badge = row
            .spans
            .iter()
            .find(|s| s.content.contains("bash"))
            .expect("badge span");
        assert!(
            badge.style.add_modifier.contains(Modifier::DIM),
            "badge must be dim: {badge:?}"
        );
        let cmd = row
            .spans
            .iter()
            .find(|s| s.content.contains("git push"))
            .expect("command span");
        assert!(
            !cmd.style.add_modifier.contains(Modifier::DIM),
            "command must not be dim: {cmd:?}"
        );
        // Still reads as one sentence when the styling is stripped.
        let flat: String = row.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            flat.contains("approved bash · git push -u origin feat/x"),
            "{flat}"
        );
    }

    /// A plain system note is untouched — no badge hunting in ordinary text.
    #[test]
    fn an_ordinary_note_is_still_one_span() {
        let mut m = ChatMsg::for_test(Role::System, "reconnected to agent");
        m.severity = Severity::Info;
        let mut lines = Vec::new();
        push_message(&mut lines, &m, 0, &ANSI, false, 80);
        assert_eq!(lines.first().expect("row").spans.len(), 1);
    }

    /// Four ranks on one row: verb, DIM badge, bright command, DIM intent. The
    /// intent is model-written and may not match what ran, so it must never be
    /// the brightest thing an operator reads back.
    #[test]
    fn a_receipt_dims_the_trailing_intent_too() {
        let mut m = ChatMsg::for_test(
            Role::System,
            "approved bash · git push -u origin feat/x  (publish the branch)",
        );
        m.severity = Severity::Success;
        let mut lines = Vec::new();
        push_message(&mut lines, &m, 0, &ANSI, false, 120);
        let row = lines.first().expect("one row");
        let intent = row
            .spans
            .iter()
            .find(|s| s.content.contains("publish the branch"))
            .expect("intent span");
        assert!(
            intent.style.add_modifier.contains(Modifier::DIM),
            "intent must be dim: {intent:?}"
        );
        let cmd = row
            .spans
            .iter()
            .find(|s| s.content.contains("git push"))
            .expect("command span");
        assert!(
            !cmd.style.add_modifier.contains(Modifier::DIM),
            "command must stay bright: {cmd:?}"
        );
        assert!(
            !cmd.content.contains("publish"),
            "intent must not ride inside the command span: {cmd:?}"
        );
    }

    /// A parenthesis inside the command is part of the command, not an intent
    /// note. Splitting on the wrong paren would dim real shell syntax and make
    /// the receipt lie about what ran.
    #[test]
    fn a_paren_inside_the_command_is_not_treated_as_intent() {
        let mut m = ChatMsg::for_test(
            Role::System,
            "approved bash · awk '{print $1}' f && (cd x && ls)",
        );
        m.severity = Severity::Success;
        let mut lines = Vec::new();
        push_message(&mut lines, &m, 0, &ANSI, false, 120);
        let row = lines.first().expect("one row");
        let cmd = row
            .spans
            .iter()
            .find(|s| s.content.contains("awk"))
            .expect("command span");
        assert!(
            cmd.content.contains("(cd x && ls)"),
            "the shell subshell must stay in the command: {cmd:?}"
        );
    }
}
