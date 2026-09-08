//! The status line and its footer segments.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::super::app::{App, SPINNER};

/// Separator between status-bar segments.
pub(super) const FOOTER_SEP: &str = " · ";

/// Below this many columns the status bar drops the steering hint and keeps
/// the numbers.
pub(super) const STATUS_FULL_MIN_WIDTH: u16 = 100;

/// Longest joined tool-name list the `AUTO:` badge will spell out before it
/// falls back to a bare count. Sized so the badge cannot crowd out the rest of
/// the status bar on a narrow terminal — two typical tool names fit.
pub(super) const AUTO_NAMES_MAX: usize = 24;

pub(super) fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let (msg, style) = if let Some(req) = &app.hitl {
        // Surface the auto-deny clock: the gate expires approvals after
        // DEFAULT_TIMEOUT (300s). Reuse `created_at` rather than tracking new
        // state; the status bar redraws on each blink deadline so it ticks.
        // The tool name goes here so the operator always knows WHAT they are
        // being asked to approve at any width; the decision keys live in the
        // framed modal / inline row, which is where the user is looking.
        let remaining = crate::hitl::gate::DEFAULT_TIMEOUT
            .as_secs()
            .saturating_sub(req.created_at.elapsed().as_secs());
        (
            format!("⏳ approve {} · auto-deny in {remaining}s", req.tool_name),
            Style::default().fg(Color::Yellow),
        )
    } else if app.streaming {
        let spin = SPINNER[app.spinner % SPINNER.len()];
        // The steering hint is the first thing to drop when the row is tight:
        // it is advice, and everything to its right is state.
        let msg = if area.width >= STATUS_FULL_MIN_WIDTH {
            format!("{spin} generating… · type to steer · Ctrl+C to cancel")
        } else {
            format!("{spin} generating…")
        };
        (msg, theme.accent)
    } else {
        let ctx = if app.context_task_id.is_some() {
            " · context kept"
        } else {
            ""
        };
        (format!("ready{ctx}"), theme.muted)
    };
    let mut spans = vec![
        Span::styled(format!(" {} ", app.agent), theme.badge),
        Span::raw("  "),
    ];
    // Auto-approval visibility (#8 / proposal 2). All three auto-approval
    // paths now show a badge, not just the global `auto_approve`:
    //   - `auto_approve`               → ` AUTO `   (every tool, session)
    //   - `session_tool_allow` (N>0)   → ` AUTO:N ` (N tools muted via [a])
    //   - `auto_reads`                 → ` READS `  (read_file auto-approved)
    // Pure display; no behaviour change. Fixes the "AUTO badge vanished in a
    // new session" illusion where `[a]`-muted tools left no visible trace.
    if app.auto_approve {
        spans.push(Span::styled(
            " AUTO ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("  "));
    } else if !app.session_tool_allow.is_empty() {
        // Name the muted tools when they fit. `AUTO:2` said something was
        // muted but never what, and nothing else would tell you either —
        // so the operator could not check whether a grant they did not mean
        // to make was still in force. `/auto off` revokes them.
        let mut names: Vec<&str> = app.session_tool_allow.iter().map(String::as_str).collect();
        names.sort_unstable();
        let joined = names.join(",");
        let label = if joined.chars().count() <= AUTO_NAMES_MAX {
            format!(" AUTO:{joined} ")
        } else {
            format!(" AUTO:{} ", names.len())
        };
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("  "));
    }
    if app.auto_reads {
        spans.push(Span::styled(
            " READS ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("  "));
    }
    if let Some(meta) = &app.channel {
        // Id only. The chip used to append `meta.state`, a persisted channel
        // lifecycle word refreshed at just two points, next to the live turn
        // state a few columns to its right — so the bar routinely read
        // `⏵ 019ff831:working   ready`. Two sources for one fact; the live one
        // wins and the stale one is gone (#940).
        let short: String = meta.id.chars().take(8).collect();
        spans.push(Span::styled(format!(" ⏵ {short} "), theme.accent));
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(msg, style));

    // Glass Box observability: tokens · cost · ctx · timer.
    //
    // Budgeted against the room actually left on the row. This used to be
    // assembled at full length and handed to the terminal, which clipped the
    // overflow — and what fell off the right edge was the context bar, the one
    // figure here that changes what you do next.
    let timer = app.turn_started.map(|t0| {
        let secs = t0.elapsed().as_secs();
        format!("{}m{:02}s · esc=stop", secs / 60, secs % 60)
    });
    let reserved: usize = spans
        .iter()
        .map(|s| s.content.chars().count())
        .sum::<usize>()
        + timer
            .as_deref()
            .map_or(0, |t| t.chars().count() + FOOTER_SEP.chars().count());
    let obs = footer_segments(
        app.turn_in,
        app.turn_out,
        app.session_in,
        app.session_out,
        app.ctx_tokens,
        &app.pricing,
        app.budget_usd,
        usize::from(area.width).saturating_sub(reserved + FOOTER_SEP.chars().count()),
    );
    if !obs.is_empty() {
        spans.push(Span::raw(FOOTER_SEP));
        spans.push(Span::styled(obs, theme.muted));
    }
    if let Some(t) = timer {
        spans.push(Span::styled(format!("{FOOTER_SEP}{t}"), theme.muted));
    }

    let right_hint: Option<(String, Style)> = if app.scroll_back > 0 {
        Some((
            format!("↑ {} lines · ⬇ to bottom", app.scroll_back),
            theme.muted,
        ))
    } else if app.esc_hint {
        let hint = if app.streaming {
            "ESC again to cancel"
        } else {
            "ESC again to clear"
        };
        Some((hint.to_string(), theme.muted))
    } else if app.ctrl_c_hint {
        Some(("Ctrl+C again to quit".to_string(), theme.muted))
    } else {
        None
    };

    if let Some((hint_text, hint_style)) = right_hint {
        let hint_display = format!(" {} ", hint_text);
        let hint_width = hint_display.chars().count() as u16;
        let left_width: u16 = spans.iter().map(|s| s.content.chars().count() as u16).sum();
        let bar_width = area.width;
        // Only right-align if there's room; otherwise skip the hint.
        if bar_width > left_width + hint_width {
            let pad = bar_width - left_width - hint_width;
            spans.push(Span::raw(" ".repeat(pad as usize)));
            spans.push(Span::styled(
                hint_display,
                hint_style.add_modifier(Modifier::DIM),
            ));
        }
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Format a token count with thousands separator (e.g. 1240 → "1,240").
pub(super) fn fmt_tok(n: u64) -> String {
    if n >= 1_000_000 {
        format!(
            "{},{:03},{:03}",
            n / 1_000_000,
            (n / 1_000) % 1_000,
            n % 1_000
        )
    } else if n >= 1_000 {
        format!("{},{:03}", n / 1_000, n % 1_000)
    } else {
        n.to_string()
    }
}

/// Pure footer formatter: `"{turn}/{sess} tok · {cost} · ctx <bar> N%"`,
/// trimmed to `budget` columns.
///
/// Cost is the SESSION estimate, not the turn's: the turn's usage is zero for
/// the whole time a reply is streaming, so the bar read `$0.000 est` next to a
/// six-figure session token count — which parses as "this was free". When a cap
/// is set the same figure renders as `spent / cap` instead of printing two
/// different costs side by side. `—` when the model is unpriced; the ctx part
/// is omitted when no window is known.
///
/// Segments drop right-to-left as `budget` shrinks — token pair first, then
/// cost — because the context bar is the only number here that changes what
/// you do next.
#[allow(clippy::too_many_arguments)]
pub(super) fn footer_segments(
    turn_in: u64,
    turn_out: u64,
    sess_in: u64,
    sess_out: u64,
    ctx_tokens: u64,
    pricing: &crate::cmd::agent::cli::footer::Pricing,
    budget_usd: Option<f64>,
    budget: usize,
) -> String {
    use crate::cmd::agent::cli::footer::{
        CTX_BAR_WIDTH, UsageCounts, context_pct, ctx_bar, turn_cost,
    };

    let toks = format!(
        "{}/{} tok",
        fmt_tok(turn_in + turn_out),
        fmt_tok(sess_in + sess_out)
    );

    let spent = turn_cost(
        pricing,
        &UsageCounts {
            input: sess_in,
            output: sess_out,
        },
    );
    let cost = match (spent, budget_usd) {
        (Some(c), Some(cap)) => format!("${c:.2} / ${cap:.2}"),
        (Some(c), None) => format!("${c:.3} est"),
        (None, Some(cap)) => format!("/ ${cap:.2}"),
        // No per-token price for this model (local runtimes, and any registry
        // entry added without `--input-cost`). An em dash in the same slot as
        // "$0.125 est" read as "this turn was free" rather than "we cannot
        // price it" — same field, two meanings, no way to tell them apart
        // (#940).
        (None, None) => "no price".to_string(),
    };

    let ctx = match pricing.window {
        Some(w) if w > 0 => {
            let pct = context_pct(ctx_tokens, w);
            format!("ctx {} {}%", ctx_bar(pct, CTX_BAR_WIDTH), pct)
        }
        _ => String::new(),
    };

    for parts in [
        [toks.as_str(), cost.as_str(), ctx.as_str()].as_slice(),
        [cost.as_str(), ctx.as_str()].as_slice(),
        [ctx.as_str()].as_slice(),
    ] {
        let s = parts
            .iter()
            .filter(|p| !p.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(FOOTER_SEP);
        if s.chars().count() <= budget {
            return s;
        }
    }
    // Nothing fits: print nothing rather than a mangled half-number.
    String::new()
}

#[cfg(test)]
mod footer_fmt_tests {
    use super::footer_segments;
    use crate::cmd::agent::cli::footer::Pricing;

    /// Enough room that these assertions test the formatting, not the trimming.
    const WIDE: usize = 200;

    #[test]
    fn shows_tokens_and_names_the_reason_when_unpriced() {
        let s = footer_segments(1240, 0, 1240, 0, 0, &Pricing::default(), None, WIDE);
        assert!(s.contains("1,240 tok") || s.contains("1240 tok"));
        // Was an em dash, which sat in the same slot as "$0.125 est" and so
        // read as a cost of zero rather than an absent price (#940).
        assert!(s.contains("no price"), "got: {s}");
    }

    #[test]
    fn shows_cost_and_ctx_when_priced() {
        let p = Pricing {
            in_per_1k: Some(0.003),
            out_per_1k: Some(0.015),
            window: Some(100_000),
        };
        let s = footer_segments(1000, 1000, 1000, 1000, 32_000, &p, None, WIDE);
        assert!(s.contains("$0.018"));
        assert!(s.contains("32%"));
        assert!(!s.contains(" / $")); // no budget suffix when budget is None
    }

    #[test]
    fn shows_budget_suffix_when_cap_set() {
        let p = Pricing {
            in_per_1k: Some(3.0),
            out_per_1k: Some(15.0),
            window: None,
        };
        // session 1000/1000 → $18.00 spent, cap $20.00
        let s = footer_segments(1000, 1000, 1000, 1000, 0, &p, Some(20.0), WIDE);
        assert!(s.contains("$18.00 / $20.00"), "got: {s}");
        // unpriced model → spent omitted, cap still shown
        let s2 = footer_segments(
            1000,
            1000,
            1000,
            1000,
            0,
            &Pricing::default(),
            Some(20.0),
            WIDE,
        );
        assert!(s2.contains("/ $20.00"), "got: {s2}");
        assert!(!s2.contains("$18.00"));
    }

    #[test]
    fn cost_is_the_session_not_the_turn() {
        let p = Pricing {
            in_per_1k: Some(3.0),
            out_per_1k: Some(15.0),
            window: None,
        };
        // Mid-stream: the turn's usage hasn't been reported yet. The bar used
        // to price the turn, so it read "$0.000 est" beside a session total.
        let s = footer_segments(0, 0, 1000, 1000, 0, &p, None, WIDE);
        assert!(s.contains("$18.000 est"), "got: {s}");
    }

    #[test]
    fn narrow_bar_keeps_the_context_gauge_and_drops_the_rest() {
        let p = Pricing {
            in_per_1k: Some(0.003),
            out_per_1k: Some(0.015),
            window: Some(100_000),
        };
        let full = footer_segments(1000, 1000, 1000, 1000, 91_000, &p, None, WIDE);
        let tight = footer_segments(1000, 1000, 1000, 1000, 91_000, &p, None, 20);
        assert!(full.len() > tight.len());
        assert!(tight.contains("91%"), "got: {tight}");
        assert!(!tight.contains("tok"), "got: {tight}");
        assert!(tight.chars().count() <= 20, "got: {tight}");
    }
}

/// #940: the observability footer must never render a figure whose blank case
/// is indistinguishable from a real one.
#[cfg(test)]
mod footer_cost_tests {
    use super::footer_segments;
    use crate::cmd::agent::cli::footer::Pricing;

    /// A model with no per-token price used to render an em dash in the exact
    /// slot where a priced model renders `$0.125 est` — so "we cannot price
    /// this" and "this cost nothing" were the same pixel. Two agents side by
    /// side in a multiplexer is where it bit.
    #[test]
    fn an_unpriced_model_says_so_rather_than_going_blank() {
        let unpriced = Pricing {
            in_per_1k: None,
            out_per_1k: None,
            window: Some(32_000),
        };
        let out = footer_segments(10, 20, 100, 200, 3_762, &unpriced, None, 200);
        assert!(
            out.contains("no price"),
            "an unpriced model must name the reason, got: {out}"
        );
        assert!(
            !out.contains('\u{2014}'),
            "the bare em dash reads as a cost of zero, got: {out}"
        );
    }

    /// Control: pricing present still produces a number, so the fix above did
    /// not simply replace the cost segment.
    #[test]
    fn a_priced_model_still_shows_the_estimate() {
        let priced = Pricing {
            in_per_1k: Some(3.0),
            out_per_1k: Some(15.0),
            window: Some(32_000),
        };
        let out = footer_segments(10, 20, 1_000, 1_000, 3_762, &priced, None, 200);
        assert!(out.contains('$'), "priced model must show a cost: {out}");
        assert!(!out.contains("no price"), "got: {out}");
    }
}

/// #940: the status bar rendered a persisted channel-lifecycle word next to the
/// live turn state, so it routinely read `⏵ 019ff831:working   ready`.
#[cfg(test)]
mod status_chip_tests {
    use super::render_status;
    use crate::cmd::agent::cli::app::App;
    use crate::cmd::agent::cli::persist::ChannelMeta;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn the_channel_chip_states_the_id_and_leaves_the_state_word_to_the_live_one() {
        let mut app = App::test_fixture();
        app.channel = Some(ChannelMeta {
            id: "019ff831dead".into(),
        });
        let mut term = Terminal::new(TestBackend::new(120, 1)).unwrap();
        term.draw(|f| render_status(f, &app, f.area())).unwrap();
        let dump = term.backend().to_string();

        assert!(dump.contains("019ff831"), "chip must show the id: {dump}");
        assert!(dump.contains("ready"), "live turn state stays: {dump}");
        assert!(
            !dump.contains("019ff831:"),
            "the chip must not append a second state word: {dump}"
        );
    }
}
