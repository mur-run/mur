//! Ratatui rendering: transcript pane, approval panel, input box, status bar.
//!
//! One submodule per region of the frame: `band` (the live transcript and its
//! flush to scrollback), `message` (one message's lines), `chooser`, `rail`,
//! `status`, `hitl`. This root owns the layout and the two popups that sit on
//! the composer.

mod approval;
mod band;
mod band_window;
mod chooser;
mod hitl;
mod message;
mod rail;
mod status;
#[cfg(test)]
mod theme_paint_tests;

pub use band::{flush_finished, flush_settled};
pub(super) use hitl::{HitlChoice, hitl_choices};

use approval::{PanelForm, docked_rows, render_approval};
use band::render_transcript;
use chooser::render_chooser_band;
use rail::{fleet_rail_height, render_fleet_rail};
use status::render_status;

/// The docked rows, for tests outside `ui`.
#[cfg(test)]
pub(super) fn docked_rows_for_test(app: &App, total_h: u16, input_h: u16) -> (u16, u16, u16, bool) {
    let (c, p, h, form) = approval::docked_rows(app, total_h, input_h);
    (c, p, h, form.is_some())
}

/// The status bar alone, for tests outside `ui` that assert on the footer.
#[cfg(test)]
pub(super) fn render_status_for_test(f: &mut Frame, app: &App, area: Rect) {
    render_status(f, app, area);
}

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph};

use super::app::App;
use super::complete;

/// Footer hint shown at the bottom of the full-screen transcript overlay
/// (Ctrl+O). Enter and Esc both return to chat; Ctrl+D quits outright, same
/// as the composer — the overlay never lets a keypress fall through
/// unhandled into the input box.
const OVERLAY_HINT: &str = " ↑↓ PgUp PgDn Home End scroll · Enter or Esc return · Ctrl+D quit ";

/// Rows the composer spends on chrome: its top rule plus the blank row under
/// the text (`app::COMPOSER_PAD_BELOW`).
pub(super) const INPUT_CHROME_ROWS: u16 = 1 + super::app::COMPOSER_PAD_BELOW;

/// Composer height when the input is empty (one text row plus the chrome;
/// the status bar sits directly beneath). Typing grows it up to
/// `INPUT_H_MAX`, which leaves seven text rows.
pub(super) const INPUT_H_MIN: u16 = 1 + INPUT_CHROME_ROWS;

pub(super) const INPUT_H_MAX: u16 = 7 + INPUT_CHROME_ROWS;

/// Draw the whole UI for one frame, then scrub control characters from every
/// cell. Scrubbing the finished buffer, not each string, means no widget —
/// transcript, HITL card, chooser, status — can let one through.
pub fn render(f: &mut Frame, app: &mut App) {
    render_frame(f, app);
    super::scrub::scrub_buffer(f.buffer_mut());
}

fn render_frame(f: &mut Frame, app: &mut App) {
    // Full-screen transcript overlay (Ctrl+O) takes over the whole frame and
    // owns every keypress (see `overlay_key_action` in `mod.rs`'s
    // `handle_event`) — nothing else renders underneath it this frame.
    if app.overlay_open {
        render_overlay(f, app);
        return;
    }
    let total_h = f.area().height;
    let input_lines = app.input.lines().len() as u16;
    let input_height = (input_lines + INPUT_CHROME_ROWS).clamp(INPUT_H_MIN, INPUT_H_MAX);
    let rail_h = fleet_rail_height(app);
    // The approval panel is docked above the composer, sized after the rows
    // that are never given up (status bar, composer at its current height)
    // and after the rail. It is the only decision surface: the step card's
    // marker has no keys, so "an open gate is always visible" holds by
    // construction — in the status-only form the status bar carries it.
    //
    // The agent chooser (suggested replies) is a band of its own too — never
    // a Clear-overlay popup — so it can't cover the reply the user must read
    // to choose. The slash-command menu keeps the compact popup (the user is
    // typing, not reading). The chooser and the proposal chip step aside
    // while a gate is open; `docked_rows` is shared with `band_inner_rows`.
    let (chooser_h, panel_h, chip_h, form) = docked_rows(app, total_h, input_height);
    let gated = form.is_some();
    let chip = super::proposal::chip_line(app).filter(|_| chip_h > 0);
    // The compact form outranks the history: the transcript may go to zero.
    let transcript_min = match form {
        Some(PanelForm::Compact | PanelForm::StatusOnly) => 0,
        Some(_) => 1,
        None => 3,
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(transcript_min),
            Constraint::Length(rail_h),
            Constraint::Length(chooser_h),
            Constraint::Length(panel_h),
            Constraint::Length(chip_h),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .split(f.area());

    render_transcript(f, app, chunks[0]);
    if rail_h > 0 {
        render_fleet_rail(f, app, chunks[1]);
    }
    if chooser_h > 0 {
        render_chooser_band(f, app, chunks[2]);
    } else if !gated && app.ssh.card.is_none() {
        render_completion(f, app, chunks[5]);
    }
    if let Some(form) = form {
        render_approval(f, app, chunks[3], form, std::time::Instant::now());
    } else if let Some(card) = app.ssh.card.as_deref() {
        super::ssh_card::render::draw(f, card, app.theme, chunks[3]);
    }
    if let Some((what, hint)) = chip {
        render_chip(f, app, chunks[4], &what, &hint);
    }
    f.render_widget(&app.input, chunks[5]);
    render_status(f, app, chunks[6]);
}

/// One row: the proposal (always shown in full — principle C) and, dimmed,
/// which key does what right now.
fn render_chip(f: &mut Frame, app: &App, area: Rect, what: &str, hint: &str) {
    let line = Line::from(vec![
        Span::styled(format!(" {what}"), app.theme.accent),
        Span::styled(format!("  {hint}"), app.theme.muted),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// Draw the full-screen transcript overlay (Ctrl+O): the plain-text
/// transcript (native select/copy/search works because we never leave raw
/// mode or the alt-screen — this is just another ratatui frame) plus a
/// footer hint. Scroll uses its own `overlay_scroll` (lines up from the
/// bottom), driven by `apply_overlay_scroll`; this clamps it and records the
/// page height for the next PgUp/PgDn.
fn render_overlay(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let text = app.overlay_text.as_deref().unwrap_or("");
    let total_lines = u16::try_from(text.lines().count()).unwrap_or(u16::MAX);
    // The bordered block eats a row top and bottom; measuring the outer
    // height hid the last two transcript lines under the border.
    let visible = chunks[0].height.saturating_sub(2);
    let max_scroll = total_lines.saturating_sub(visible);
    let scroll = app.overlay_scroll.min(max_scroll);
    // Write the clamp back so PgDn after overshooting the top moves at once.
    app.overlay_scroll = scroll;
    app.overlay_page = visible;
    // scroll_back counts lines up from the bottom; ratatui's Paragraph scroll
    // counts down from the top, so invert it.
    let top_offset = max_scroll.saturating_sub(scroll);

    f.render_widget(Clear, area);
    let title = if scroll > 0 {
        format!(" transcript · {scroll} lines below · End to follow ")
    } else {
        " transcript ".to_string()
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    f.render_widget(
        Paragraph::new(Text::raw(text))
            .block(block)
            .scroll((top_offset, 0)),
        chunks[0],
    );

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            OVERLAY_HINT,
            app.theme.muted.add_modifier(Modifier::ITALIC),
        ))),
        chunks[1],
    );
}

/// Draw completion menu list anchored just above the input box.
/// No-op when the menu is closed or empty.
fn render_completion(f: &mut Frame, app: &App, input_area: Rect) {
    let Some(state) = &app.completion else {
        return;
    };
    if state.items.is_empty() {
        return;
    }
    let theme = app.theme;

    // The suggested-reply chooser (`spaced`) is a Claude-Code-style option list:
    // each option gets a label line, an optional dimmed description line, and a
    // blank spacer row so the choices breathe. The slash-command menu stays
    // one-line-per-row and reverse-highlighted.
    let rows: Vec<ListItem> = state
        .items
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if state.spaced {
                // Numbered option: "N  label" + a dimmed, aligned description +
                // a spacer. The number is a quiet affordance for digit-select.
                let mut lines = vec![Line::from(vec![
                    Span::styled(format!("{}  ", i + 1), theme.muted),
                    Span::styled(c.display.clone(), theme.text),
                ])];
                if !c.desc.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!("   {}", c.desc), // align under the label (past "N  ")
                        theme.muted,
                    )));
                }
                lines.push(Line::default()); // spacer between options
                ListItem::new(lines)
            } else {
                // The row in force reads in `ok` with a ✔, the way a settings
                // list says where you are before you move.
                let is_current = state.current == Some(i);
                let mut spans = vec![Span::styled(
                    c.display.clone(),
                    if is_current {
                        theme.ok.add_modifier(Modifier::BOLD)
                    } else {
                        theme.muted
                    },
                )];
                if is_current {
                    spans.push(Span::styled(" ✔", theme.ok));
                }
                if !c.desc.is_empty() {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(c.desc.clone(), theme.muted));
                }
                ListItem::new(Line::from(spans))
            }
        })
        .collect();

    // Height = actual rendered lines of the shown items (+2 borders). Spaced
    // items span several lines each, so a flat item count would clip them.
    let shown = rows.len().min(complete::MAX_MENU_ROWS);
    let content_lines: usize = rows.iter().take(shown).map(ListItem::height).sum();
    let popup_height = (content_lines as u16).saturating_add(2);

    // Anchor above the input box, then clamp to the frame so a popup taller
    // than the space above the input (short / stacked-pane terminals) can never
    // render out of bounds — ratatui panics on an out-of-buffer index.
    let y = input_area.y.saturating_sub(popup_height);
    let popup_area = Rect {
        x: input_area.x,
        y,
        width: input_area.width,
        height: popup_height,
    }
    .intersection(f.area());

    let title = if state.spaced {
        " 1-9 pick · ↑↓ move · Enter accept · Esc close "
    } else {
        " ↑↓ move · Tab accept · Esc close "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.accent)
        .padding(Padding::horizontal(state.spaced as u16))
        .title(title)
        .title_style(theme.muted);

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected));

    // Spaced items are multi-line; a full reverse bar would paint the spacer and
    // description too. Mark the selection with a caret + accent-bold label
    // instead. The slash menu keeps its compact reverse highlight.
    let (highlight_style, highlight_symbol) = if state.spaced {
        (theme.accent.add_modifier(Modifier::BOLD), "❯ ")
    } else {
        (Style::default().add_modifier(Modifier::REVERSED), "")
    };

    f.render_widget(Clear, popup_area);
    f.render_stateful_widget(
        List::new(rows)
            .block(block)
            .highlight_style(highlight_style)
            .highlight_symbol(highlight_symbol),
        popup_area,
        &mut list_state,
    );
}
