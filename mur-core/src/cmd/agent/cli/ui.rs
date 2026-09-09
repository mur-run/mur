//! Ratatui rendering: transcript pane, input box, status bar, HITL modal.
//!
//! One submodule per region of the frame: `band` (the live transcript and its
//! flush to scrollback), `message` (one message's lines), `chooser`, `rail`,
//! `status`, `hitl`. This root owns the layout and the two popups that sit on
//! the composer.

mod band;
mod chooser;
mod hitl;
mod message;
mod rail;
mod status;

pub use band::flush_finished;
pub(super) use hitl::hitl_scroll_step;

use band::render_transcript;
use chooser::{chooser_band_height, render_chooser_band};
use hitl::render_hitl;
use rail::{fleet_rail_height, render_fleet_rail};
use status::render_status;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph};

use super::app::App;
use super::complete;

/// Footer hint shown at the bottom of the full-screen transcript overlay
/// (Ctrl+O). Enter and Esc both return to chat; Ctrl+D quits outright, same
/// as the composer — the overlay never lets a keypress fall through
/// unhandled into the input box.
const OVERLAY_HINT: &str = " press Enter or Esc to return · Ctrl+D quit ";

/// Rows the composer spends on chrome: its top rule plus the blank row under
/// the text (`app::COMPOSER_PAD_BELOW`).
const INPUT_CHROME_ROWS: u16 = 1 + super::app::COMPOSER_PAD_BELOW;

/// Composer height when the input is empty (one text row plus the chrome;
/// the status bar sits directly beneath). Typing grows it up to
/// `INPUT_H_MAX`, which leaves seven text rows.
pub(super) const INPUT_H_MIN: u16 = 1 + INPUT_CHROME_ROWS;

const INPUT_H_MAX: u16 = 7 + INPUT_CHROME_ROWS;

/// Draw the whole UI for one frame.
pub fn render(f: &mut Frame, app: &mut App) {
    // Full-screen transcript overlay (Ctrl+O) takes over the whole frame and
    // owns every keypress (see `overlay_key_action` in `mod.rs`'s
    // `handle_event`) — nothing else renders underneath it this frame.
    if app.overlay_open {
        render_overlay(f, app);
        return;
    }
    let input_lines = app.input.lines().len() as u16;
    let input_height = (input_lines + INPUT_CHROME_ROWS).clamp(INPUT_H_MIN, INPUT_H_MAX);
    // The agent chooser (suggested replies) renders as its own layout band
    // between transcript and composer — never a Clear-overlay popup — so it
    // can't cover the reply the user must read to choose. The slash-command
    // menu keeps the compact popup (the user is typing, not reading).
    let chooser_h = chooser_band_height(app, f.area().height, input_height);
    let rail_h = fleet_rail_height(app);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(rail_h),
            Constraint::Length(chooser_h),
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
    } else {
        render_completion(f, app, chunks[3]);
    }
    f.render_widget(&app.input, chunks[3]);
    render_status(f, app, chunks[4]);

    // The centered modal is the fallback whenever the approval's inline row on
    // a step card is not actually visible. Key it on VISIBILITY recomputed per
    // frame (`hitl_inline_visible`), never on whether the runtime sent a
    // step_id and never on a cached flag: the gate commonly fires before the
    // card exists, and a card that is live when the gate opens can be flushed
    // into frozen scrollback while it is still open. Both used to leave the
    // operator with neither surface. The invariant: an open gate always has at
    // least one place the operator can see it.
    if let Some(hitl) = app
        .hitl
        .clone()
        .filter(|h| !app.hitl_inline_visible(h.step_id.as_deref()))
    {
        let (used, shown) = render_hitl(
            f,
            app.theme,
            &hitl,
            app.hitl_grant_confirm,
            app.input_text().is_empty(),
            app.hitl_scroll,
        );
        app.hitl_scroll = used;
        // The renderer is the only place that knows the box height, so it hands
        // the page size back rather than the key handler guessing one.
        app.hitl_page = shown;
    }
}

/// Draw the full-screen transcript overlay (Ctrl+O): the plain-text
/// transcript (native select/copy/search works because we never leave raw
/// mode or the alt-screen — this is just another ratatui frame) plus a
/// footer hint. Scroll follows the normal `scroll_back`/PageUp/PageDown
/// state so the same keys work here as in the regular transcript pane.
fn render_overlay(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let text = app.overlay_text.as_deref().unwrap_or("");
    let total_lines = text.lines().count() as u16;
    let visible = chunks[0].height;
    let max_scroll = total_lines.saturating_sub(visible);
    let scroll = app.scroll_back.min(max_scroll);
    // scroll_back counts lines up from the bottom; ratatui's Paragraph scroll
    // counts down from the top, so invert it.
    let top_offset = max_scroll.saturating_sub(scroll);

    f.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).title(" transcript ");
    f.render_widget(
        Paragraph::new(Text::raw(text))
            .block(block)
            .scroll((top_offset, 0)),
        chunks[0],
    );

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            OVERLAY_HINT,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
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
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                lines.push(Line::default()); // spacer between options
                ListItem::new(lines)
            } else {
                let mut spans = vec![Span::styled(c.display.clone(), theme.muted)];
                if !c.desc.is_empty() {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        c.desc.clone(),
                        Style::default().fg(Color::DarkGray),
                    ));
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
