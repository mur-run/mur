//! The fleet rail: one row per member while a fleet run is live.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Text};
use ratatui::widgets::Paragraph;

use super::super::app::App;

/// Rows the fleet rail paints: one collapsed line, plus a capped member list
/// when someone is blocked, plus one more row for the "… N more" truncation
/// notice when the member list doesn't fit. Must always equal
/// `rail_lines(view, _).len()` — see `the_rail_height_matches_painted_lines`.
/// A working fleet is not news; a stalled one is.
pub fn rail_height_for(view: &crate::cmd::agent::cli::fleet_rail::RailView) -> u16 {
    use crate::cmd::agent::cli::fleet_rail::{MAX_EXPANDED_ROWS, MemberState};
    let blocked = view
        .members
        .iter()
        .any(|m| matches!(m.state, MemberState::Blocked { .. }));
    if !blocked {
        return 1;
    }
    let shown = view.members.len().min(MAX_EXPANDED_ROWS) as u16;
    let truncated = view.members.len() > MAX_EXPANDED_ROWS;
    1 + shown + u16::from(truncated)
}

/// Height of the rail band for the current app state; 0 when `--fleet` is off.
pub fn fleet_rail_height(app: &App) -> u16 {
    app.fleet_view().map(rail_height_for).unwrap_or(0)
}

/// The fleet rail's content as plain lines: the head line, a capped member
/// list, and a truncation notice when the list doesn't fit. Pulled out of
/// `render_fleet_rail` so `rail_height_for` has something concrete to be
/// tested against instead of a number nothing checks — the truncation notice
/// is the thing most likely to silently fall off the bottom again.
pub(super) fn rail_lines(
    view: &crate::cmd::agent::cli::fleet_rail::RailView,
    theme: &'static crate::cmd::agent::cli::theme::Theme,
) -> Vec<Line<'static>> {
    use crate::cmd::agent::cli::fleet_rail::{MAX_EXPANDED_ROWS, MemberState};
    let mut lines: Vec<Line> = Vec::new();

    let head = match &view.notice {
        Some(n) => format!("{}  {n}", view.jobs_line),
        None => view.jobs_line.clone(),
    };
    lines.push(Line::styled(head, theme.muted.add_modifier(Modifier::BOLD)));

    if rail_height_for(view) > 1 {
        for m in view.members.iter().take(MAX_EXPANDED_ROWS) {
            let (body, style) = match &m.state {
                MemberState::Blocked { summary, .. } => (format!("blocked: {summary}"), theme.warn),
                MemberState::Working { tool, since } => (
                    match tool {
                        Some(t) => format!("working ({}) · {t}", elapsed(*since)),
                        None => format!("working ({})", elapsed(*since)),
                    },
                    theme.accent,
                ),
                MemberState::Done => ("done".to_string(), theme.ok),
                MemberState::Failed => ("failed".to_string(), theme.error),
            };
            let glyph = m.state.glyph();
            lines.push(Line::styled(
                format!("  {:<10} {glyph} {body}", m.agent),
                style,
            ));
        }
        let extra = view.members.len().saturating_sub(MAX_EXPANDED_ROWS);
        if extra > 0 {
            lines.push(Line::styled(format!("  … {extra} more"), theme.muted));
        }
    }

    lines
}

pub(super) fn render_fleet_rail(f: &mut Frame, app: &App, area: Rect) {
    let Some(view) = app.fleet_view() else {
        return;
    };
    f.render_widget(
        Paragraph::new(Text::from(rail_lines(view, app.theme))),
        area,
    );
}

/// "2m" / "1h04m" — elapsed since a member last changed state. Shown instead
/// of a staleness verdict: a runtime that died mid-turn shows a growing
/// number rather than a state we guessed.
pub(super) fn elapsed(since: chrono::DateTime<chrono::Utc>) -> String {
    let secs = (chrono::Utc::now() - since).num_seconds().max(0);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

#[cfg(test)]
mod fleet_rail_layout_tests {
    use super::super::band::band_capacity;
    use super::*;
    use crate::cmd::agent::cli::fleet_rail::{MemberRow, MemberState, RailView};

    fn view(blocked: usize) -> RailView {
        RailView {
            jobs_line: "fleet · dev   job 0/1".into(),
            members: (0..blocked)
                .map(|i| MemberRow {
                    agent: format!("m{i}"),
                    state: MemberState::Blocked {
                        summary: "approve".into(),
                        hitl_id: format!("h{i}"),
                    },
                })
                .collect(),
            notice: None,
        }
    }

    #[test]
    fn rail_is_one_row_until_someone_is_blocked() {
        assert_eq!(rail_height_for(&view(0)), 1);
        assert_eq!(rail_height_for(&view(1)), 2);
        assert_eq!(rail_height_for(&view(3)), 4);
    }

    #[test]
    fn the_expanded_rail_is_capped() {
        use crate::cmd::agent::cli::fleet_rail::MAX_EXPANDED_ROWS;
        assert_eq!(
            rail_height_for(&view(50)),
            // +1 for the member rows (capped), +1 for the "… N more" notice
            // that only appears once the list is actually truncated.
            1 + MAX_EXPANDED_ROWS as u16 + 1,
            "an unbounded rail would eat the transcript, and a truncated one \
             must still show its own truncation notice"
        );
    }

    #[test]
    fn the_live_band_gives_back_exactly_what_the_rail_takes() {
        // The guard for the one dangerous coupling: band_inner_rows decides
        // when transcript content is flushed to scrollback, so it must account
        // for every row the rail paints or the flush drifts from the picture.
        // Tested on the pure arithmetic so no App and no test-only seam in
        // production code are needed.
        let viewport_h = 20u16;
        let input_h = 3u16;
        let without = band_capacity(viewport_h, input_h, 0, 0);
        let with_rail = band_capacity(viewport_h, input_h, 0, rail_height_for(&view(3)));
        assert_eq!(without - with_rail, rail_height_for(&view(3)));
    }

    #[test]
    fn the_rail_height_matches_what_render_actually_paints() {
        // rail_height_for is a number computed independently of rail_lines;
        // nothing ties them together except this test. Without it, a line
        // added to (or removed from) rail_lines silently desyncs the height
        // from the paint — exactly how the truncation notice went missing.
        use crate::cmd::agent::cli::fleet_rail::MAX_EXPANDED_ROWS;
        let theme = crate::cmd::agent::cli::theme::resolve_skin("dark");
        for blocked in [0, 1, MAX_EXPANDED_ROWS, MAX_EXPANDED_ROWS + 3] {
            let v = view(blocked);
            assert_eq!(
                rail_lines(&v, theme).len() as u16,
                rail_height_for(&v),
                "mismatch at {blocked} blocked members"
            );
        }
    }
}
