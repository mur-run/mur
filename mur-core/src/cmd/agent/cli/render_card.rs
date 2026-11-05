//! Ratatui lines for one in-transcript tool-call step card.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::call_summary::{
    ARG_HINT_MIN, INTENT_MIN_COLS, arg_hint, elide_tail_cols, fmt_duration, hint_budget,
    tool_description, width_of,
};
use super::step::{ARGS_MAX_LINES, StepCard, StepState};
use super::theme::Theme;

/// Maximum output lines shown inside a card before a "…+N more" truncation hint.
pub const OUTPUT_MAX_LINES: usize = 20;

/// Columns the `  → ` lead-in of a result gist occupies, charged against the
/// row before deciding whether a trailing intent still fits.
const GIST_CHROME_COLS: usize = 4;

/// Columns the ` (` … `)` around an inline command occupy on a row that
/// leads with the intent.
const COMMAND_CHROME_COLS: usize = 3;

/// Indent of the command when it did not fit beside the intent and moved to
/// its own row: under the name, past the glyph.
const COMMAND_ROW_INDENT: &str = "    ";

/// Maximum changed/context rows shown on a collapsed edit card. The path header
/// is always retained; the full unbounded patch stays one Ctrl+O away.
const COLLAPSED_DIFF_PREVIEW_LINES: usize = 20;

/// Columns the ` — ` before a failure's cause occupies.
const CAUSE_CHROME_COLS: usize = 3;

/// Fewest columns a failure's cause is cut to; a narrower row wraps instead
/// of losing the reason.
const CAUSE_MIN_COLS: usize = 16;

/// The awaiting-approval marker row on a step card.
pub(super) const AWAITING_MARKER: &str = "  ⏳ awaiting approval";

/// Append a bounded edit preview and a clear route to the complete patch.
///
/// Tool cards live in a transcript, so allowing a large write to consume the
/// conversation makes both the agent's conclusion and the next approval hard
/// to find. The Ctrl+O overlay is already a full-screen, natively scrollable
/// representation and deliberately renders edit diffs without a cap.
fn push_collapsed_diff_preview(
    out: &mut Vec<Line<'static>>,
    diff_lines: Vec<Line<'static>>,
    theme: &'static Theme,
) {
    let path_rows = usize::from(diff_lines.first().is_some_and(|line| {
        line.spans
            .first()
            .is_some_and(|span| !span.content.starts_with("  "))
    }));
    let shown = path_rows + COLLAPSED_DIFF_PREVIEW_LINES;
    let hidden = diff_lines.len().saturating_sub(shown);
    out.extend(diff_lines.into_iter().take(shown));
    if hidden > 0 {
        out.push(Line::styled(
            format!(" … {hidden} more · Ctrl+O full diff"),
            theme.muted.add_modifier(Modifier::DIM),
        ));
    }
}

/// Turn a `StepCard` into renderable `Line`s for the transcript.
///
/// `expanded` controls verbosity. Collapsed (the default) shows a single
/// summary line — glyph, tool name, arg hint, a one-line result gist, and
/// duration — so a transcript of many tool calls stays scannable. Expanded
/// shows the full args + result (still bounded by `ARGS_MAX_LINES` /
/// `OUTPUT_MAX_LINES`). Errors and pending HITL rows always render in both
/// modes so nothing actionable is hidden. Full detail for a collapsed card is
/// always available in the Ctrl+O transcript overlay.
///
/// `width` is the terminal's column count: the header's arg hint is budgeted
/// from it rather than clipped at a fixed column, which used to cut a command
/// short at 40 characters and leave most of a wide row empty.
pub fn card_lines(
    card: &StepCard,
    theme: &'static Theme,
    expanded: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let budget = hint_budget(width);

    let ink = HeaderInk::for_card(card, theme);

    // ── Header: glyph · name · intent (command) → gist · duration / tokens ──
    //
    // When the call carries a `description`, the human-readable intent leads
    // in the accent weight and the command trails as a DIM parenthetical: in
    // a scrolling transcript the eye wants "what was this for" first, and the
    // literal command is the receipt it can check against. When the row is
    // too tight to hold both, the command is NOT dropped — it moves to its
    // own dim line beneath, because the command is the only thing that ran.
    // Without a description the row is the plain `glyph name hint` it always
    // was.
    let meta = header_meta(card);
    let desc = tool_description(card);
    let auto_tag = if card.auto_approved {
        Span::styled(" [auto]", theme.muted.add_modifier(Modifier::DIM))
    } else {
        Span::raw("")
    };
    let gist = (!expanded && card.error.is_none())
        .then(|| result_gist(card, budget))
        .flatten();
    let gist_cols = gist
        .as_deref()
        .map_or(0, |g| width_of(g) + GIST_CHROME_COLS);
    let mut header_spans = Vec::new();
    let mut command_row: Option<String> = None;
    match desc {
        Some(desc) => {
            let prefix = format!("{} {} ", card.glyph(), card.name);
            let fixed = width_of(&prefix) + gist_cols + width_of(&meta);
            let desc_room = usize::from(width)
                .saturating_sub(fixed)
                .max(INTENT_MIN_COLS);
            let intent = elide_tail_cols(&desc, desc_room);
            let lead = format!("{prefix}{intent}");
            ink.push_glyph_and_name(&mut header_spans, card);
            header_spans.push(Span::styled(intent, ink.body));
            let room = usize::from(width)
                .saturating_sub(width_of(&lead) + gist_cols + width_of(&meta))
                .saturating_sub(COMMAND_CHROME_COLS);
            let full_hint = arg_hint(card, budget);
            if !full_hint.is_empty() {
                if room >= ARG_HINT_MIN {
                    header_spans.push(Span::styled(
                        format!(" ({})", arg_hint(card, room)),
                        theme.muted.add_modifier(Modifier::DIM),
                    ));
                } else {
                    // Its own row has the whole width, less indent and parens.
                    let own_row = usize::from(width)
                        .saturating_sub(COMMAND_ROW_INDENT.len() + COMMAND_CHROME_COLS)
                        .max(ARG_HINT_MIN);
                    command_row = Some(arg_hint(card, own_row));
                }
            }
        }
        None => {
            let hint = arg_hint(card, budget);
            ink.push_glyph_and_name(&mut header_spans, card);
            header_spans.push(Span::styled(hint, ink.body));
        }
    }
    if let Some(gist) = &gist {
        header_spans.push(Span::styled(format!("  → {gist}"), theme.muted));
    }
    header_spans.push(Span::styled(meta, theme.muted));
    header_spans.push(auto_tag);
    // A failure says its cause once, on the header, in `text`: the glyph and
    // the name already carry `error`, and the wrapper chain the runtime sends
    // (`tool error: tool execution failed: …`) repeated what they said.
    let failure = card.error.as_deref().map(super::failure::parse);
    if let Some(f) = &failure {
        let used: usize = header_spans.iter().map(|s| width_of(&s.content)).sum();
        let room = usize::from(width)
            .saturating_sub(used + CAUSE_CHROME_COLS)
            .max(CAUSE_MIN_COLS);
        header_spans.push(Span::styled(
            format!(" — {}", elide_tail_cols(&f.cause, room)),
            theme.text,
        ));
    }
    out.push(Line::from(header_spans));
    if let Some(cmd) = command_row {
        out.push(Line::from(Span::styled(
            format!("{COMMAND_ROW_INDENT}({cmd})"),
            theme.muted.add_modifier(Modifier::DIM),
        )));
    }

    // Collapsed cards keep the transcript compact, but edits need a visible
    // receipt: include their diff and let the transcript viewport scroll it.
    if !expanded {
        if card.error.is_none()
            && let Some(diff_lines) = super::diff::edit_diff_lines(&card.name, &card.args, theme)
        {
            push_collapsed_diff_preview(&mut out, diff_lines, theme);
        }
        push_error_and_hitl(&mut out, card, theme);
        return out;
    }

    // ── Args: diff for edit tools, else bounded JSON ─────────────────────────
    if let Some(diff_lines) = super::diff::edit_diff_lines(&card.name, &card.args, theme) {
        out.extend(diff_lines);
    } else if !card.args.is_null() {
        let pretty = serde_json::to_string_pretty(&card.args).unwrap_or_default();
        let total_lines = pretty.lines().count();
        for l in pretty.lines().take(ARGS_MAX_LINES) {
            out.push(Line::styled(format!(" {l}"), theme.muted));
        }
        if total_lines > ARGS_MAX_LINES {
            out.push(Line::styled(
                format!(" … +{} more", total_lines - ARGS_MAX_LINES),
                theme.muted.add_modifier(Modifier::DIM),
            ));
        }
    }

    // ── Result / error (bounded) ──────────────────────────────────────────────
    if let Some(line) = error_line(card, theme) {
        out.push(line);
    }

    if !card.output.is_empty() {
        let output_line_count = card.output.lines().count();
        for l in card.output.lines().take(OUTPUT_MAX_LINES) {
            out.push(Line::styled(format!(" {l}"), theme.text));
        }
        let shown = output_line_count.min(OUTPUT_MAX_LINES);
        // Show "+N more" either when we clipped locally OR the runtime
        // already truncated the output (full_len > what we received).
        let total = if card.truncated {
            card.full_len
        } else {
            output_line_count
        };
        if card.truncated || output_line_count > OUTPUT_MAX_LINES {
            out.push(Line::styled(
                format!(" … +{} more", total.saturating_sub(shown)),
                theme.muted.add_modifier(Modifier::DIM),
            ));
        }
    }

    // ── Inline HITL approval (P2) ────────────────────────────────────────────
    if card.awaiting_hitl {
        out.push(hitl_row(theme));
    }

    out
}

/// The `fix:` row under a failed card: the remediation the runtime embedded
/// in the error, lifted out so it can be read and copied whole. `None` when
/// the card did not fail or the runtime named no fix. The cause itself is on
/// the header (see `card_lines`). Shown in both collapsed and expanded modes.
fn error_line(card: &StepCard, theme: &'static Theme) -> Option<Line<'static>> {
    let fix = super::failure::parse(card.error.as_deref()?).fix?;
    Some(Line::from(Span::styled(
        format!("{}fix: {fix}", super::failure::FIX_INDENT),
        theme.muted,
    )))
}

/// The marker a card carries while its call waits on a decision. It has no
/// keys: the decision is made in the docked approval panel, which is always
/// on screen, so a second surface could only disagree with it.
fn hitl_row(theme: &'static Theme) -> Line<'static> {
    Line::from(Span::styled(AWAITING_MARKER, theme.muted))
}

/// Collapsed-card tail: error line (if any) + pending-HITL row (if any). The
/// success gist is folded into the header instead.
fn push_error_and_hitl(out: &mut Vec<Line<'static>>, card: &StepCard, theme: &'static Theme) {
    if let Some(line) = error_line(card, theme) {
        out.push(line);
    }
    if card.awaiting_hitl {
        out.push(hitl_row(theme));
    }
}

/// One-line gist of a successful tool result for the collapsed header. Best
/// effort: for JSON results, count the obvious result set (`count`, or the
/// length of a `results`/`matches`/`items` array); otherwise fall back to a
/// short inline value or a line/char count. `None` when there's nothing useful
/// to say (empty output).
/// Trailing ` · 77ms / 100 tokens` metadata. Duration first because it is
/// always known once the call finishes; tokens only once the runtime has
/// counted what the model actually received (after compression), so an older
/// runtime — or a still-running call — shows the duration alone.
/// Ink for a tool row's header (spec §4): on a successful row only the `✔`
/// takes colour and nothing is bold, so a page of them leaves the eye on the
/// `●` role label and the agent's sentences. A failure keeps its weight.
struct HeaderInk {
    glyph: Style,
    name: Style,
    body: Style,
}

impl HeaderInk {
    fn for_card(card: &StepCard, theme: &'static Theme) -> Self {
        let error = theme.error.add_modifier(Modifier::BOLD);
        match card.state {
            StepState::Done => Self {
                glyph: theme.ok,
                name: theme.muted,
                body: theme.text,
            },
            StepState::Error => Self {
                glyph: error,
                name: error,
                body: theme.text,
            },
            StepState::Running | StepState::Yielded => Self {
                glyph: theme.accent,
                name: theme.muted,
                body: theme.text,
            },
        }
    }

    /// `✔ bash ` as two styled spans plus their separators.
    fn push_glyph_and_name(&self, spans: &mut Vec<Span<'static>>, card: &StepCard) {
        spans.push(Span::styled(card.glyph().to_string(), self.glyph));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(card.name.clone(), self.name));
        spans.push(Span::raw(" "));
    }
}

fn header_meta(card: &StepCard) -> String {
    let mut meta = String::new();
    if let Some(ms) = card.duration_ms {
        meta.push_str(&format!(" · {}", fmt_duration(ms)));
    }
    if let Some(tokens) = card.tokens {
        meta.push_str(if card.duration_ms.is_some() {
            " / "
        } else {
            " · "
        });
        meta.push_str(&format!("{tokens} tokens"));
    }
    meta
}

fn result_gist(card: &StepCard, budget: usize) -> Option<String> {
    let out = card.output.trim();
    if out.is_empty() {
        return None;
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(out) {
        if let Some(n) = v.get("count").and_then(serde_json::Value::as_u64) {
            return Some(format!("{n} results"));
        }
        for key in ["results", "matches", "items"] {
            if let Some(arr) = v.get(key).and_then(serde_json::Value::as_array) {
                return Some(format!("{} {key}", arr.len()));
            }
        }
    }
    // Non-JSON (or unrecognised shape): single short line inline, else counts.
    let lines = out.lines().count();
    if lines <= 1 {
        let end = out.floor_char_boundary(budget);
        return Some(if out.len() > budget {
            format!("{}…", &out[..end])
        } else {
            out.to_string()
        });
    }
    Some(format!("{lines} lines"))
}

#[cfg(test)]
mod tests;
