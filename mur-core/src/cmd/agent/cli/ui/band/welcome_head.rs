//! The welcome banner as the head of the live band (#2358).
//!
//! Printed above the viewport, the banner had the whole empty transcript band
//! between it and the composer. Painted here instead, bottom-aligned, it sits
//! on the composer with the blank rows above it; the first overflow commits
//! it to scrollback ahead of any message, so it scrolls away the way printed
//! output does.

use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::Rect;
use ratatui::text::Line;

use super::super::super::app::App;
use super::super::super::welcome::WelcomeHead;
use super::{band_rows, emit};

/// The head's lines, or none when the banner is not in the band. `gap` adds
/// the blank row that separates it from a message below.
pub(in super::super) fn lines(app: &App, gap: bool) -> Vec<Line<'static>> {
    if app.welcome_head == WelcomeHead::Off {
        return Vec::new();
    }
    let mut lines = app.welcome_banner();
    if gap {
        lines.push(Line::default());
    }
    lines
}

/// Commit the head to scrollback when it is due: on overflow — it is the
/// oldest row in the band, so it leaves before any message — or when a
/// handover asked for it. `rest` is the rows the messages take.
pub(in super::super) fn flush<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    rest: u32,
    cap: u32,
    width: u16,
) -> std::io::Result<()> {
    let head = lines(app, app.flushed_upto < app.messages.len());
    let due = match app.welcome_head {
        WelcomeHead::Off => false,
        WelcomeHead::Commit => true,
        WelcomeHead::Live => u32::from(band_rows(app.theme, head.clone(), width)) + rest > cap,
    };
    if due {
        emit(terminal, head, app.theme.inner_padding as u16, width)?;
        app.welcome_head = WelcomeHead::Off;
    }
    Ok(())
}

/// Where the band paints `total` rows of content. While the welcome is in it
/// the content is bottom-aligned so the banner sits on the composer; otherwise
/// `area` unchanged.
pub(in super::super) fn content_area(app: &App, area: Rect, total: u32) -> Rect {
    if app.welcome_head == WelcomeHead::Off {
        return area;
    }
    let short = u16::try_from(total).unwrap_or(u16::MAX).min(area.height);
    let pad = area.height - short;
    Rect {
        y: area.y + pad,
        height: short,
        ..area
    }
}
