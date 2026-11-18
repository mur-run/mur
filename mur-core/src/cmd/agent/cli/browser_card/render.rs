//! Card lines. Pure: tests read the text without a terminal. Wrapping and
//! fitting reuse the SSH card's helpers so both cards size the same way.

use ratatui::text::{Line, Span};

use super::super::ssh_card::render::{fit, inner_width, rows as rows_of};
use super::super::theme::Theme;
use super::state::{Phase, State};
use crate::cmd::browser::request_card::front::{self, Card, Offer, Reach, Status};

const CURSOR: &str = "›";

pub fn title(c: &State) -> String {
    format!(" browser · {} ", c.shown.request.site.origin())
}

pub fn hint(c: &State) -> &'static str {
    match (&c.phase, &c.shown.card) {
        (Phase::Working, _) => "working…",
        (Phase::LoggingIn, _) => "Enter logged in · Esc cancel (nothing is saved)",
        (Phase::Saved { .. }, _) => "Enter/Esc close",
        (_, Card::Login { .. }) => {
            "↑↓ pick · Enter use · r replace · l log in · d dismiss · Esc close"
        }
        (_, Card::Reach { .. }) => "a allow · d dismiss · Esc close",
        (_, Card::NoLiveEntry) => "Esc close",
    }
}

pub fn status_text(s: &Status) -> String {
    match s {
        Status::SavedNotTried => "saved, not yet tried".into(),
        Status::AgentReported(t) => format!(
            "agent-reported · {}",
            t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M")
        ),
    }
}

pub fn lines(c: &State, t: &'static Theme) -> Vec<Line<'static>> {
    let r = &c.shown.request;
    let mut out = vec![
        Line::from(format!("{} asks for {}", c.shown.agent, r.site.origin())),
        Line::styled(format!("“{}”", r.reason), t.muted),
        Line::from(""),
    ];
    match &c.phase {
        Phase::LoggingIn => {
            out.push(Line::from(
                "a browser window is open — log in there, then press Enter here.",
            ));
        }
        Phase::Saved { profile, status } => {
            out.push(Line::from(format!("saved login “{profile}”")));
            out.push(Line::styled(status_text(status), t.muted));
        }
        _ => body(c, t, &mut out),
    }
    if let Some(step) = front::port_step(&c.shown) {
        out.push(Line::from(""));
        out.push(Line::styled(step.warning, t.warn));
        for cmd in step.commands {
            out.push(Line::styled(format!("  {cmd}"), t.warn));
        }
    }
    if let Some(n) = &c.note {
        out.push(Line::from(""));
        out.push(Line::styled(n.clone(), t.accent));
    }
    out
}

fn body(c: &State, t: &'static Theme, out: &mut Vec<Line<'static>>) {
    match &c.shown.card {
        Card::NoLiveEntry => out.push(Line::from(
            "this agent has no live browser yet — set one up with /browser live",
        )),
        Card::Reach { reach, .. } => out.push(Line::from(match reach {
            Reach::Allowed => "the live browser already reaches it; a allows the request".into(),
            Reach::NeedsHost { missing } => format!("allowing adds {}", missing.join(", ")),
            Reach::Unscoped { mode } => {
                format!(
                    "the browser is not host-scoped ({mode}); the agent's network policy decides"
                )
            }
            Reach::Disabled { why } => format!("not available: {why}"),
        })),
        Card::Login {
            rows,
            login_again,
            login_offer,
            ..
        } => {
            if rows.is_empty() {
                out.push(Line::styled("no saved login covers this site", t.muted));
            }
            for (i, row) in rows.iter().enumerate() {
                let on = i == c.selected;
                let disabled = matches!(row.offer, Offer::Disabled { .. });
                let style = if disabled {
                    t.muted
                } else if on {
                    t.accent
                } else {
                    t.text
                };
                out.push(Line::from(vec![
                    Span::styled(if on { CURSOR } else { " " }.to_string(), t.accent),
                    Span::styled(format!(" {}  {}", row.name, row.domains.join(", ")), style),
                ]));
                out.push(Line::styled(
                    format!("    {}", offer_text(&row.offer)),
                    t.muted,
                ));
                if let Some(at) = &row.session_elsewhere {
                    out.push(Line::styled(
                        format!(
                            "    logged in at {at} — may have no session for {}",
                            c.shown.request.site.host()
                        ),
                        t.warn,
                    ));
                }
                if !row.shared_with.is_empty() {
                    out.push(Line::styled(
                        format!("    shared with {}", row.shared_with.join(", ")),
                        t.muted,
                    ));
                }
            }
            out.push(Line::from(""));
            let on = c.selected == c.login_at();
            let label = if login_again.exists {
                format!(" l  log in again as “{}”", login_again.name)
            } else {
                format!(" l  log in and save as “{}”", login_again.name)
            };
            out.push(Line::from(vec![
                Span::styled(if on { CURSOR } else { " " }.to_string(), t.accent),
                Span::styled(label, if on { t.accent } else { t.text }),
            ]));
            if !matches!(login_offer, Offer::Offered) {
                out.push(Line::styled(
                    format!("    {}", login_offer_text(login_offer)),
                    t.muted,
                ));
            }
        }
    }
}

/// The login line's offer: what saving the login would do to the binding.
fn login_offer_text(o: &Offer) -> String {
    match o {
        Offer::ReplaceOnly { .. } => format!("{} — press r to log in", offer_text(o)),
        _ => offer_text(o),
    }
}

pub fn offer_text(o: &Offer) -> String {
    match o {
        Offer::AlreadyBound => "already in use — nothing to change".into(),
        Offer::Offered => "use this login".into(),
        Offer::AddsHosts { missing } => format!("using it also adds {}", missing.join(", ")),
        Offer::ReplaceOnly { drops, adds, .. } => {
            let mut s = format!("only by replacing {}", drops.join(", "));
            if !adds.is_empty() {
                s.push_str(&format!("; adds {}", adds.join(", ")));
            }
            s
        }
        Offer::Disabled { why } => format!("not available: {why}"),
    }
}

pub fn height(
    c: &State,
    theme: &'static Theme,
    width: u16,
    total_h: u16,
    avail: u16,
    pct: u16,
) -> u16 {
    let w = inner_width(width);
    let body: u16 = lines(c, theme)
        .iter()
        .fold(0u16, |acc, l| acc.saturating_add(rows_of(l, w)));
    body.saturating_add(3).min(total_h * pct / 100).min(avail)
}

pub fn draw(f: &mut ratatui::Frame, c: &State, theme: &'static Theme, area: ratatui::layout::Rect) {
    use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
    if area.height < 3 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.accent)
        .title(Span::styled(title(c), theme.accent));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let body_h = inner.height.saturating_sub(1);
    let body = fit(lines(c, theme), inner.width, body_h);
    f.render_widget(
        Paragraph::new(body).wrap(Wrap { trim: false }),
        ratatui::layout::Rect {
            height: body_h,
            ..inner
        },
    );
    f.render_widget(
        Paragraph::new(Line::styled(hint(c), theme.muted)),
        ratatui::layout::Rect {
            y: inner.y + body_h,
            height: 1,
            ..inner
        },
    );
}
