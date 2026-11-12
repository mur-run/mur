//! Card lines. Pure: the band hands in an area, this returns what to draw,
//! so tests read the text without a terminal.

use ratatui::text::{Line, Span};

use super::super::theme::Theme;
use super::state::{CardState, Step};
use crate::cmd::agent::ssh::setup::{SpawnChange, Verdict};

const MARK_ON: &str = "[x]";
const MARK_OFF: &str = "[ ]";
const CURSOR: &str = "›";

pub fn title(c: &CardState) -> String {
    let n = match c.step {
        Step::Key => 1,
        Step::Host => 2,
        Step::Summary => 3,
    };
    format!(" SSH access · {} · step {n}/3 ", c.plan.host.canonical())
}

pub fn hint(c: &CardState) -> &'static str {
    if c.editing_user {
        return "type the login user · Enter done";
    }
    match c.step {
        Step::Key => "↑↓ pick · Enter next · u user · Esc cancel",
        Step::Host => "↑↓ move · Space I compared this · paste a fingerprint · Enter next · ← back",
        Step::Summary => "Enter approve · c consent · ← back · Esc cancel",
    }
}

fn sel<'a>(on: bool, t: &'static Theme) -> (Span<'a>, ratatui::style::Style) {
    if on {
        (Span::styled(format!("{CURSOR} "), t.accent), t.emphasis)
    } else {
        (Span::raw("  "), t.text)
    }
}

pub fn lines(c: &CardState, t: &'static Theme) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    if let Some(r) = c.origin.request() {
        out.push(Line::styled("The agent asked, in its own words:", t.muted));
        out.push(Line::styled(format!("  │ {}", r.reason), t.text));
    }
    let user = if c.user.is_empty() {
        "(not set)"
    } else {
        c.user.as_str()
    };
    let cursor = if c.editing_user { "▏" } else { "" };
    out.push(Line::styled(format!("login user: {user}{cursor}"), t.muted));
    match c.step {
        Step::Key => key_lines(c, t, &mut out),
        Step::Host => host_lines(c, t, &mut out),
        Step::Summary => summary_lines(c, t, &mut out),
    }
    if let Some(n) = &c.note {
        out.push(Line::styled(n.clone(), t.warn));
    }
    out
}

fn key_lines(c: &CardState, t: &'static Theme, out: &mut Vec<Line<'static>>) {
    out.push(Line::styled(
        "Which key may the agent sign with?",
        t.emphasis,
    ));
    let keys = c.keys();
    if keys.is_empty() {
        out.push(Line::styled(
            "  no key in your ssh-agent and no ~/.ssh/*.pub found",
            t.warn,
        ));
    }
    for (i, (k, loaded)) in keys.into_iter().enumerate() {
        let (mark, style) = sel(i == c.key_idx, t);
        let state = if loaded {
            "loaded"
        } else {
            "not loaded — Enter runs ssh-add"
        };
        out.push(Line::from(vec![
            mark,
            Span::styled(format!("{}  {}", k.fingerprint, k.comment), style),
            Span::styled(format!("  ({state})"), t.muted),
        ]));
    }
}

fn verdict_text(v: &Verdict) -> &'static str {
    match v {
        Verdict::Trusted => "matches your ~/.ssh/known_hosts — already trusted",
        Verdict::Unknown { downgraded: false } => {
            "not in your ~/.ssh/known_hosts — compare with the operator's published fingerprint"
        }
        Verdict::Unknown { downgraded: true } => {
            "matches your ~/.ssh/known_hosts, but this agent can write that file (or may run \
             without its sandbox), so the match proves nothing — compare it"
        }
        Verdict::Differs => {
            "your ~/.ssh/known_hosts stores a DIFFERENT key: the host was rebuilt, or the \
             connection is intercepted"
        }
    }
}

fn host_lines(c: &CardState, t: &'static Theme, out: &mut Vec<Line<'static>>) {
    let style = match c.plan.verdict {
        Verdict::Trusted => t.ok,
        Verdict::Differs => t.error,
        Verdict::Unknown { .. } => t.warn,
    };
    out.push(Line::styled(verdict_text(&c.plan.verdict), style));
    for (i, k) in c.plan.scanned.iter().enumerate() {
        let (mark, st) = sel(i == c.host_idx, t);
        let tick = if c.plan.verdict == Verdict::Trusted {
            if c.plan.matched.contains(&k.fingerprint) {
                "[✓]"
            } else {
                "   "
            }
        } else if !c.ticks_offered() {
            "   "
        } else if c.ticked.contains(&k.fingerprint) {
            MARK_ON
        } else {
            MARK_OFF
        };
        out.push(Line::from(vec![
            mark,
            Span::styled(
                format!("{tick} {}  {}", k.entry.key_type, k.fingerprint),
                st,
            ),
        ]));
    }
    match &c.pasted {
        Some(fp) => out.push(Line::styled(format!("pasted: {fp}"), t.text)),
        None if c.plan.verdict != Verdict::Trusted => out.push(Line::styled(
            "paste the operator's fingerprint to match it",
            t.muted,
        )),
        None => {}
    }
    if c.plan.verdict == Verdict::Differs {
        out.push(Line::styled(
            format!(
                "or update ~/.ssh/known_hosts yourself (`ssh-keygen -R {}`), then reopen /ssh",
                c.plan.host.host()
            ),
            t.muted,
        ));
    }
}

fn summary_lines(c: &CardState, t: &'static Theme, out: &mut Vec<Line<'static>>) {
    out.push(Line::styled("Approving writes:", t.emphasis));
    match c.approval() {
        Ok(a) => {
            out.push(Line::styled(format!("  key       {}", a.key_fp), t.text));
            for h in &a.hosts {
                out.push(Line::styled(
                    format!("  host key  {}", h.host_key_fp),
                    t.text,
                ));
            }
        }
        Err(e) => out.push(Line::styled(format!("  not ready: {e}"), t.warn)),
    }
    if c.plan.spawn == Some(SpawnChange::AddSsh) {
        out.push(Line::styled(
            "  spawn     add `ssh` to the spawn allowlist",
            t.text,
        ));
    }
    out.push(Line::styled(
        format!("  one restart of '{}'", c.plan.agent),
        t.text,
    ));
    if c.plan.spawn == Some(SpawnChange::NoneToStrict) {
        let mark = if c.spawn_consented { MARK_ON } else { MARK_OFF };
        out.push(Line::styled(
            format!("{mark} process permissions change: `none` → `strict` with `ssh` (c)"),
            t.warn,
        ));
        if let Some(es) = &c.plan.exec_surface {
            for l in es.consent_text().lines() {
                out.push(Line::styled(format!("    {l}"), t.muted));
            }
        }
        out.push(Line::styled(
            "    Or cancel, and create a separate agent for SSH work so this one keeps `none`.",
            t.muted,
        ));
    }
}

/// Rows the card takes: its lines, two borders, one hint row, capped at the
/// approval panel's share of the screen and at what is available.
pub fn height(c: &CardState, theme: &'static Theme, total_h: u16, avail: u16, pct: u16) -> u16 {
    let want = lines(c, theme).len() as u16 + 3;
    want.min(total_h * pct / 100).min(avail)
}

/// Draw the card into `area`.
pub fn draw(
    f: &mut ratatui::Frame,
    c: &CardState,
    theme: &'static Theme,
    area: ratatui::layout::Rect,
) {
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
    let mut body = lines(c, theme);
    // Keep the newest step content visible when the screen is short.
    if body.len() as u16 > body_h {
        body.drain(..body.len() - usize::from(body_h));
    }
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
