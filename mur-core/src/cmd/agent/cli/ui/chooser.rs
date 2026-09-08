//! The suggested-reply chooser: its own layout band between transcript and
//! composer, never a popup over the reply the operator must read.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Padding};

use super::super::app::App;

/// Height of the chooser band, or 0 when no agent chooser is open (slash
/// menu stays a popup). Full spaced rows (label + optional desc + spacer)
/// when they fit above the composer while leaving the transcript at least
/// `MIN_TRANSCRIPT_ROWS`; otherwise compact one-line rows; never taller
/// than the space available (the List scrolls the selection into view).
pub(super) const MIN_TRANSCRIPT_ROWS: u16 = 3;

/// Share of the viewport the transcript keeps when the chooser is open,
/// in percent.
pub(super) const TRANSCRIPT_FLOOR_PCT: u16 = 40;

pub(super) fn chooser_band_height(app: &App, total_h: u16, input_height: u16) -> u16 {
    let Some(state) = &app.completion else {
        return 0;
    };
    if !state.spaced || state.items.is_empty() {
        return 0;
    }
    let chrome = input_height + 1; // composer + status line
    let full: u16 = state
        .items
        .iter()
        .map(|c| 2 + u16::from(!c.desc.is_empty())) // label + spacer (+ desc)
        .sum::<u16>()
        .saturating_add(2); // borders
    let compact = (state.items.len() as u16).saturating_add(2);
    // Prefer the readable floor; fall back to the hard minimum only when the
    // floor would squeeze the chooser below its compact form. The chooser is
    // what the operator must act on, so it never loses this trade.
    let roomy = total_h
        .saturating_sub(chrome)
        .saturating_sub((total_h * TRANSCRIPT_FLOOR_PCT / 100).max(MIN_TRANSCRIPT_ROWS));
    let tight = total_h.saturating_sub(chrome + MIN_TRANSCRIPT_ROWS);
    let available = if roomy >= compact { roomy } else { tight };
    // Take `compact` exactly when the spaced form does not fit — padding the
    // band out to `available` spends rows on nothing.
    let auto = if full <= available {
        full
    } else {
        compact.min(available).max(3)
    };
    // Ctrl+↑/↓ while the chooser is open grows/shrinks the band on top of
    // the auto height, clamped so the transcript keeps its minimum rows.
    (i32::from(auto) + i32::from(app.chooser_grow))
        .clamp(3, i32::from(tight.max(MIN_TRANSCRIPT_ROWS))) as u16
}

/// Draw the agent chooser into its own layout band. Falls back to compact
/// one-line rows ("N label — desc") when the band is shorter than the full
/// spaced form.
pub(super) fn render_chooser_band(f: &mut Frame, app: &App, area: Rect) {
    let Some(state) = &app.completion else {
        return;
    };
    let theme = app.theme;
    let full: u16 = state
        .items
        .iter()
        .map(|c| 2 + u16::from(!c.desc.is_empty()))
        .sum::<u16>()
        .saturating_add(2);
    let compact = area.height < full;

    let rows: Vec<ListItem> = state
        .items
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if compact {
                let mut spans = vec![
                    Span::styled(format!("{} ", i + 1), theme.muted),
                    Span::styled(c.display.clone(), theme.text),
                ];
                if !c.desc.is_empty() {
                    spans.push(Span::styled(
                        format!(" — {}", c.desc),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                ListItem::new(Line::from(spans))
            } else {
                let mut lines = vec![Line::from(vec![
                    Span::styled(format!("{} ", i + 1), theme.muted),
                    Span::styled(c.display.clone(), theme.text),
                ])];
                if !c.desc.is_empty() {
                    lines.push(Line::from(Span::styled(
                        format!("   {}", c.desc),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                lines.push(Line::default());
                ListItem::new(lines)
            }
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.accent)
        .padding(Padding::horizontal(1))
        .title(" 1-9 pick · ↑↓ move · Enter accept · Esc close · Ctrl+↑↓ resize ")
        .title_style(theme.muted);

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected));

    f.render_stateful_widget(
        List::new(rows)
            .block(block)
            .highlight_style(theme.accent.add_modifier(Modifier::BOLD))
            .highlight_symbol("❯ "),
        area,
        &mut list_state,
    );
}

#[cfg(test)]
mod chooser_floor_tests {
    use super::chooser_band_height;
    use crate::cmd::agent::cli::app::App;
    use crate::cmd::agent::cli::complete::{Candidate, CompletionState};

    fn option(display: &str, desc: &str) -> Candidate {
        Candidate {
            display: display.into(),
            insert: display.into(),
            desc: desc.into(),
            has_children: false,
        }
    }

    /// Three suggested replies, each with a description — the shape from the
    /// report.
    fn app_with_three_options() -> App {
        let mut a = App::test_fixture();
        a.completion = Some(CompletionState {
            items: vec![
                option("open the PR", "wait for CI, then tag"),
                option("stronger model", "the 4B one fakes tool calls"),
                option("leave it", "change nothing"),
            ],
            selected: 0,
            spaced: true,
        });
        a
    }

    #[test]
    fn the_chooser_leaves_the_transcript_more_than_three_rows() {
        // Inline viewport is 20 rows; composer 3 + status 1 leaves 16.
        let h = chooser_band_height(&app_with_three_options(), 20, 3);
        assert!(
            h <= 8,
            "chooser took {h} rows, leaving the reply a peephole"
        );
    }

    #[test]
    fn a_short_terminal_still_gets_a_usable_chooser() {
        // The floor must yield rather than squeeze the chooser out: it is the
        // thing the operator has to act on.
        let h = chooser_band_height(&app_with_three_options(), 12, 3);
        assert!(h >= 5, "chooser unusable at {h} rows");
    }

    #[test]
    fn ctrl_up_still_reaches_the_spaced_form() {
        let mut a = app_with_three_options();
        a.chooser_grow = 6;
        assert_eq!(chooser_band_height(&a, 20, 3), 11);
    }
}
