//! The approval panel: a layout band docked between the transcript and the
//! composer (spec 2026-09-29 §3). It replaces the centred modal, which was
//! drawn over the transcript and cut through the lines that explain why the
//! agent wants the tool. Nothing is drawn over anything: the transcript band
//! shrinks to make room, exactly as it does for the suggested-reply chooser.
//!
//! The panel is the ONLY place a decision is made. The step card keeps its
//! `⏳` marker but carries no keys, so "an open gate is always visible"
//! holds by construction — including the status-only form, where the status
//! bar names the tool and the keys.

use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use serde_json::Value;

use super::super::app::App;
use super::hitl::{HitlChoice, hitl_choices};
use crate::cmd::agent::cli::call_summary::{elide_tail_cols, width_of};
use crate::cmd::agent::cli::stream::HitlRequest;
use crate::cmd::agent::cli::theme::Theme;
use crate::cmd::fleet::review::murmur::bridge::HitlOrigin;

/// At or under this much time left, the countdown turns `error`.
pub(crate) const COUNTDOWN_WARN: Duration = Duration::from_secs(30);

/// The panel never takes more than this share of the terminal's rows.
pub(super) const PANEL_MAX_PCT: u16 = 60;

/// Fewest body rows the full form is worth drawing with; below this the
/// no-body form says where the call can be read in full instead.
pub(super) const BODY_MIN_ROWS: u16 = 3;

/// Below this many rows the bordered forms cannot fit; the panel goes compact.
pub(super) const BORDERED_MIN_ROWS: u16 = 8;

/// Below this many rows even the compact form cannot fit; status only.
pub(super) const COMPACT_MIN_ROWS: u16 = 2;

/// Rows a bordered panel spends on everything but its body: two border rows,
/// and inside them one blank separator plus one row per option. The title
/// sits on the top border.
const fn chrome_rows(options: u16) -> u16 {
    2 + 1 + options
}

/// Characters of the composer's text quoted in option 4's label.
const REASON_QUOTE_CHARS: usize = 30;

/// Hint the no-body form shows in place of the call it could not fit.
pub(super) const NO_BODY_HINT: &str = "⋯ Ctrl+O shows the call";

/// Which form the panel takes in the rows it was given. Keys work the same
/// in every form, so the operator can always decide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PanelForm {
    /// Border, title, `body` rows of the call, a blank row, the options.
    Full { body: u16 },
    /// Border, title, the options — the call is one `Ctrl+O` away.
    NoBody,
    /// Two rows, no border: the title, then every option on one row.
    Compact,
    /// Nothing drawn; the status bar carries the request.
    StatusOnly,
}

impl PanelForm {
    pub(super) fn height(self, options: u16) -> u16 {
        match self {
            Self::Full { body } => chrome_rows(options) + body,
            Self::NoBody => chrome_rows(options),
            Self::Compact => COMPACT_MIN_ROWS,
            Self::StatusOnly => 0,
        }
    }
}

/// Pick the form for a panel with `body_rows` rows of call body, given
/// `avail` rows left after the status bar, composer and fleet rail, in a
/// terminal `total_h` rows tall. First that fits, in order:
///
/// - full: within `min(avail − 1, 60 % of total_h)` with ≥ 3 body rows (the
///   transcript keeps at least one row);
/// - no body: same cap, without the body;
/// - compact: `avail` ≥ 2 — the transcript gives up its rows first, because
///   the decision outranks the history;
/// - status only.
pub(super) fn panel_form(body_rows: u16, options: u16, avail: u16, total_h: u16) -> PanelForm {
    let cap = avail
        .saturating_sub(1)
        .min(total_h.saturating_mul(PANEL_MAX_PCT) / 100);
    if avail >= BORDERED_MIN_ROWS {
        let chrome = chrome_rows(options);
        let room = cap.saturating_sub(chrome);
        let body = body_rows.min(room);
        if body >= BODY_MIN_ROWS.min(body_rows.max(1)) && body > 0 {
            return PanelForm::Full { body };
        }
        if chrome <= cap {
            return PanelForm::NoBody;
        }
    }
    if avail >= COMPACT_MIN_ROWS {
        PanelForm::Compact
    } else {
        PanelForm::StatusOnly
    }
}

/// The rows the panel's form is chosen from. `None` when no gate is open.
pub(super) fn approval_form(app: &App, total_h: u16, avail: u16) -> Option<PanelForm> {
    let req = app.hitl.as_ref()?;
    let width = app.width.max(1);
    let options = hitl_choices(app.hitl_origin).len() as u16;
    let body = body_lines(req, app.theme, inner_cols(width));
    Some(panel_form(body.len() as u16, options, avail, total_h))
}

/// Rows the docked bands take above a composer `input_h` rows tall, in a
/// terminal `total_h` rows tall: `(chooser, panel, chip)`, plus the panel's
/// form. One function for the painter and for `band_inner_rows`, so the
/// rows the band keeps are exactly the rows it is given.
///
/// While a gate is open the chooser and the chip are 0: both need the
/// composer the approval has taken over, and come back when it closes.
pub(super) fn docked_rows(
    app: &App,
    total_h: u16,
    input_h: u16,
) -> (u16, u16, u16, Option<PanelForm>) {
    let rail_h = super::rail::fleet_rail_height(app);
    let avail = total_h.saturating_sub(1 + input_h + rail_h);
    let form = approval_form(app, total_h, avail);
    if let Some(form) = form {
        let options = hitl_choices(app.hitl_origin).len() as u16;
        return (0, form.height(options).min(avail), 0, Some(form));
    }
    let chooser = super::chooser::chooser_band_height(app, total_h, input_h);
    let chip = u16::from(crate::cmd::agent::cli::proposal::chip_line(app).is_some());
    (chooser, 0, chip, None)
}

/// Is the panel in its status-only form — no rows to draw in, so the status
/// bar must carry the request, its keys and its clock?
pub(super) fn status_only(app: &App, total_h: u16) -> bool {
    let input_h = (app.input.lines().len() as u16 + super::INPUT_CHROME_ROWS)
        .clamp(super::INPUT_H_MIN, super::INPUT_H_MAX);
    matches!(
        docked_rows(app, total_h, input_h).3,
        Some(PanelForm::StatusOnly)
    )
}

/// Columns inside the border.
fn inner_cols(width: u16) -> usize {
    usize::from(width.saturating_sub(4))
}

/// `m:ss` until `deadline`.
pub(crate) fn countdown(left: Duration) -> String {
    let s = left.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

/// The countdown's ink: `error` at 30 s or less, `muted` before that.
///
/// Not `warn`: the panel's border and title are `accent`, and on the `mur`
/// skin `accent` (#fbbf24) and `warn` (#f2c76a) are both gold, so a `warn`
/// clock sat on a gold border and never read as a change. `error` is the one
/// ink every skin keeps apart from `accent`.
pub(super) fn countdown_style(theme: &'static Theme, left: Duration) -> Style {
    if left <= COUNTDOWN_WARN {
        theme.error
    } else {
        theme.muted
    }
}

/// `<tool> · <target>` — the target is the path for file tools and is left
/// out when the call has none.
pub(super) fn title_text(req: &HitlRequest) -> String {
    match target_of(&req.tool_input) {
        Some(t) => format!("{} · {t}", req.tool_name),
        None => req.tool_name.clone(),
    }
}

fn target_of(input: &Value) -> Option<String> {
    ["file_path", "path"]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The body for this call, before folding:
///
/// - an edit `diff::edit_diff_lines` accepts: the diff, with the transcript
///   card's rendering and tokens (its path row is dropped — the title has it);
/// - `bash`: the command, wrapped with a hanging indent;
/// - anything else: one `key: value` row per field, strings unescaped, each
///   value cut to one row with `…`.
pub(super) fn body_lines(
    req: &HitlRequest,
    theme: &'static Theme,
    cols: usize,
) -> Vec<Line<'static>> {
    if let Some(diff) =
        crate::cmd::agent::cli::diff::edit_diff_lines(&req.tool_name, &req.tool_input, theme)
    {
        let has_path = target_of(&req.tool_input).is_some();
        return diff.into_iter().skip(usize::from(has_path)).collect();
    }
    if req.tool_name.eq_ignore_ascii_case("bash")
        && let Some(cmd) = req.tool_input.get("command").and_then(Value::as_str)
    {
        return hanging_wrap(cmd, cols, theme.text);
    }
    match &req.tool_input {
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| {
                let value = match v {
                    Value::String(s) => s.replace('\n', " ⏎ "),
                    other => other.to_string(),
                };
                let lead = format!("{k}: ");
                let room = cols.saturating_sub(width_of(&lead));
                Line::from(vec![
                    Span::styled(lead, theme.muted),
                    Span::styled(elide_tail_cols(&value, room), theme.text),
                ])
            })
            .collect(),
        Value::Null => Vec::new(),
        other => vec![Line::styled(
            elide_tail_cols(&other.to_string(), cols),
            theme.text,
        )],
    }
}

/// Indent continuation rows of a wrapped command by this many columns.
const HANGING_INDENT: usize = 2;

/// Wrap `s` to `cols`, every row after the first indented, so a long command
/// reads as one command rather than as several.
fn hanging_wrap(s: &str, cols: usize, style: Style) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for (n, src) in s.lines().enumerate() {
        let first_room = if n == 0 {
            cols
        } else {
            cols.saturating_sub(HANGING_INDENT)
        };
        let rest_room = cols.saturating_sub(HANGING_INDENT).max(1);
        let lead = if n == 0 { "" } else { "  " };
        let mut rows = super::hitl::wrap_row(src, first_room.max(1));
        let head = rows.remove(0);
        out.push(Line::styled(format!("{lead}{head}"), style));
        let tail: String = rows.concat();
        if !tail.is_empty() {
            for r in super::hitl::wrap_row(&tail, rest_room) {
                out.push(Line::styled(format!("  {r}"), style));
            }
        }
    }
    out
}

/// Fold `lines` to at most `room` rows: keep the head and the tail, replace
/// the middle with one muted `⋯ N lines hidden · Ctrl+O shows all` row.
pub(super) fn fold(
    lines: Vec<Line<'static>>,
    room: usize,
    theme: &'static Theme,
) -> Vec<Line<'static>> {
    if lines.len() <= room {
        return lines;
    }
    if room == 0 {
        return Vec::new();
    }
    let keep = room - 1;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let hidden = lines.len() - keep;
    let mut out: Vec<Line<'static>> = lines[..head].to_vec();
    out.push(Line::styled(
        format!("⋯ {hidden} lines hidden · Ctrl+O shows all"),
        theme.muted,
    ));
    out.extend_from_slice(&lines[lines.len() - tail..]);
    out
}

/// Option 4's label follows the composer: what will be sent as the reason.
///
/// The row is also where a reason is written (`Tab` or `4` moves there
/// without deciding), so the empty label says how to get there, and says
/// what Enter will do once the cursor has arrived.
pub(super) fn deny_label(origin: HitlOrigin, composer: &str, selected: bool) -> String {
    if origin == HitlOrigin::Review {
        // A member's denial has nowhere to carry composer text.
        return "No — or Esc".to_string();
    }
    let typed = composer.trim();
    if typed.is_empty() {
        return if selected {
            "No — type a reason, then Enter (or Enter for just no)".to_string()
        } else {
            "No — Tab to add a reason, or Esc".to_string()
        };
    }
    let flat = typed.replace('\n', " ");
    let quoted: String = flat.chars().take(REASON_QUOTE_CHARS).collect();
    let more = if flat.chars().count() > REASON_QUOTE_CHARS {
        "…"
    } else {
        ""
    };
    format!("No — send \"{quoted}{more}\" as the reason")
}

/// The label of one option row, without its number.
fn option_label(
    choice: HitlChoice,
    req: &HitlRequest,
    origin: HitlOrigin,
    composer: &str,
    selected: bool,
) -> String {
    match choice {
        HitlChoice::Deny => deny_label(origin, composer, selected),
        HitlChoice::Once => "Yes".to_string(),
        HitlChoice::Tool | HitlChoice::All => {
            let grant = crate::cmd::agent::cli::dest::grant_for(
                &req.tool_name,
                Some(&req.tool_input),
                req.tier(),
            );
            choice.label(&grant)
        }
    }
}

/// Option 3's word in the compact form. Not "any tool": the row is ANDed
/// with the tier ceiling and can never cover a risky call (#1878), so the
/// short form must not promise more than the long label does.
pub(super) const ALL_WORD: &str = "stop asking";

/// One option row: `▸ 3 Yes, and stop asking …  ▲`. The selected row is
/// `badge` (on `ansi`, reverse video — the one focus exception); option 3's
/// `▲` is `warn`; every other row is plain `text`.
fn option_line(
    i: usize,
    label: String,
    choice: HitlChoice,
    selected: bool,
    cols: usize,
    theme: &'static Theme,
) -> Line<'static> {
    let caret = if selected { "▸ " } else { "  " };
    let base = if selected { theme.badge } else { theme.text };
    let lead = format!("{caret}{} ", i + 1);
    let risky = choice == HitlChoice::All;
    // Keep the `▲` on the row at any width: the label gives way first.
    let label_room = cols.saturating_sub(width_of(&lead) + if risky { 2 } else { 0 });
    let label = elide_tail_cols(&label, label_room);
    let mut spans = vec![
        Span::styled(lead.clone(), base),
        Span::styled(label.clone(), base),
    ];
    if risky {
        let used = width_of(&lead) + width_of(&label);
        let pad = cols.saturating_sub(used + 2);
        spans.push(Span::styled(" ".repeat(pad), base));
        spans.push(Span::styled(" ▲", if selected { base } else { theme.warn }));
    } else if selected {
        // The focus bar spans the row, so the selection reads at a glance.
        let pad = cols.saturating_sub(width_of(&lead) + width_of(&label));
        spans.push(Span::styled(" ".repeat(pad), base));
    }
    Line::from(spans)
}

/// The compact form's one-row option list: `1 Yes · 2 edit_file · 3 any tool ▲ · 4 No`.
fn compact_options(
    req: &HitlRequest,
    choices: &[HitlChoice],
    selected: usize,
    theme: &'static Theme,
) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (i, c) in choices.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", theme.muted));
        }
        let word = match c {
            HitlChoice::Once => "Yes".to_string(),
            HitlChoice::Tool => req.tool_name.clone(),
            HitlChoice::All => ALL_WORD.to_string(),
            HitlChoice::Deny => "No".to_string(),
        };
        let style = if i == selected {
            theme.badge
        } else {
            theme.text
        };
        spans.push(Span::styled(format!("{} {word}", i + 1), style));
        if *c == HitlChoice::All {
            spans.push(Span::styled(" ▲", theme.warn));
        }
    }
    Line::from(spans)
}

/// Draw the panel in `area`, in the form `form`.
pub(super) fn render_approval(f: &mut Frame, app: &App, area: Rect, form: PanelForm, now: Instant) {
    let Some(req) = app.hitl.as_ref() else { return };
    if area.height == 0 || form == PanelForm::StatusOnly {
        return;
    }
    let theme = app.theme;
    let origin = app.hitl_origin;
    let choices = hitl_choices(origin);
    let selected = app.hitl_selected.min(choices.len() - 1);
    let left = req.remaining(now);
    let clock = countdown(left);
    let clock_style = countdown_style(theme, left);
    let title = match app.review_gate.member.as_deref() {
        Some(m) => format!("{} · review member {m}", title_text(req)),
        None => title_text(req),
    };

    if form == PanelForm::Compact {
        let cols = usize::from(area.width);
        let room = cols.saturating_sub(width_of(&clock) + 3);
        let head = Line::from(vec![
            Span::styled(
                format!(" {}", elide_tail_cols(&title, room)),
                theme.emphasis,
            ),
            Span::raw(
                " ".repeat(room.saturating_sub(width_of(&elide_tail_cols(&title, room))) + 1),
            ),
            Span::styled(clock, clock_style),
        ]);
        let rows = vec![head, compact_options(req, choices, selected, theme)];
        f.render_widget(Paragraph::new(rows), area);
        return;
    }

    let cols = inner_cols(area.width);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let PanelForm::Full { body } = form {
        let all = body_lines(req, theme, cols);
        lines.extend(fold(all, usize::from(body), theme));
        lines.push(Line::default());
    } else {
        lines.push(Line::styled(NO_BODY_HINT, theme.muted));
    }
    let composer = app.input_text();
    for (i, c) in choices.iter().enumerate() {
        let label = option_label(*c, req, origin, &composer, i == selected);
        lines.push(option_line(i, label, *c, i == selected, cols, theme));
    }

    let room = usize::from(area.width).saturating_sub(width_of(&clock) + 6);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(theme.border_type)
        .border_style(theme.accent)
        .title(Line::from(Span::styled(
            format!(" {} ", elide_tail_cols(&title, room)),
            theme.emphasis,
        )))
        .title(Line::from(Span::styled(format!(" {clock} "), clock_style)).right_aligned())
        .padding(ratatui::widgets::Padding::horizontal(1));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
#[path = "approval_tests.rs"]
mod tests;
