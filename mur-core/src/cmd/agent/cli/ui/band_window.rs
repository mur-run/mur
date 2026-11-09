//! Wrapped-row measurement for the live band, without re-wrapping it all.
//!
//! Every repaint used to wrap every unflushed line from scratch — in
//! `flush_finished` (via `band_rows` / `line_rows`) to decide what to flush, and
//! again twice in `render_transcript` (`line_count`, then `render` from row 0
//! down to the scroll offset). A running `!cmd` ticks the spinner every
//! `SPINNER_MS`, and its live card holds up to `SHELL_CARD_MAX_BYTES`, so an
//! idle-looking murmur grapheme-segmented that card ~11×/s and sat at 60% CPU
//! (#2325).
//!
//! Lines wrap independently under `Wrap { trim: false }`, so a paragraph's
//! height is the sum of its lines' heights. Those are cached by (width, line
//! content): a spinner tick changes one glyph in one line, so one line
//! re-wraps. `render_transcript` then hands ratatui only the lines on screen.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Wrap};

/// Entries kept before the cache is dropped and refilled. One entry per
/// distinct (width, line); a 256 KiB shell card is a few thousand lines, so
/// this holds several screens of transcript at a couple of widths.
// ponytail: clear-all on overflow, not LRU — a refill costs one full re-wrap.
const CACHE_MAX: usize = 50_000;

thread_local! {
    static ROWS: RefCell<HashMap<(u16, u64), u32>> = RefCell::new(HashMap::new());
}

fn content_key(line: &Line<'_>) -> u64 {
    // Style never moves a wrap point and the wrapper walks graphemes across
    // span boundaries, so the text alone decides the height.
    let mut h = DefaultHasher::new();
    for span in &line.spans {
        span.content.hash(&mut h);
    }
    h.finish()
}

/// Rows `line` wraps to at `width` columns — exactly
/// `Paragraph::new(line).wrap(Wrap { trim: false }).line_count(width)`.
pub(super) fn rows(line: &Line<'static>, width: u16) -> u32 {
    let width = width.max(1);
    let key = (width, content_key(line));
    if let Some(n) = ROWS.with(|c| c.borrow().get(&key).copied()) {
        return n;
    }
    let n = Paragraph::new(Text::from(line.clone()))
        .wrap(Wrap { trim: false })
        .line_count(width) as u32;
    ROWS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= CACHE_MAX {
            c.clear();
        }
        c.insert(key, n);
    });
    n
}

/// What `render_transcript` paints: `lines` scrolled down by `scroll` rows.
pub(super) struct Window {
    pub lines: Vec<Line<'static>>,
    pub scroll: u16,
    pub total: u32,
}

/// Cut `lines` down to the rows `[offset, offset + visible)`, where `offset`
/// follows from `scroll_back` (rows up from the tail) exactly as before.
/// `scroll_back` is clamped in place to the new maximum.
pub(super) fn window(
    lines: Vec<Line<'static>>,
    width: u16,
    visible: u16,
    scroll_back: &mut u16,
) -> Window {
    let heights: Vec<u32> = lines.iter().map(|l| rows(l, width)).collect();
    let total: u32 = heights.iter().sum();
    // Offsets stay u16 (ratatui's scroll type), as the old `line_count() as
    // u16` did.
    let max_scroll = (total.min(u32::from(u16::MAX)) as u16).saturating_sub(visible);
    *scroll_back = (*scroll_back).min(max_scroll);
    let offset = u32::from(max_scroll - *scroll_back);
    let end = offset + u32::from(visible);

    let (mut first, mut last) = (lines.len(), lines.len());
    let (mut into_first, mut top) = (0u32, 0u32);
    for (i, &h) in heights.iter().enumerate() {
        if first == lines.len() && top + h > offset {
            first = i;
            into_first = offset - top;
        }
        top += h;
        if top >= end {
            last = i + 1;
            break;
        }
    }
    let lines = if first < last {
        lines.into_iter().take(last).skip(first).collect()
    } else {
        Vec::new()
    };
    Window {
        lines,
        scroll: into_first.min(u32::from(u16::MAX)) as u16,
        total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn sample() -> Vec<Line<'static>> {
        (0..40)
            .map(|i| match i % 3 {
                0 => Line::from(format!("short {i}")),
                1 => Line::from(format!("{i} {}", "wide 中文字 word ".repeat(i % 7 + 1))),
                _ => Line::from(""),
            })
            .collect()
    }

    fn paint(lines: Vec<Line<'static>>, scroll: u16, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| {
            let p = Paragraph::new(Text::from(lines))
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0));
            f.render_widget(p, f.area());
        })
        .unwrap();
        format!("{:?}", t.backend().buffer())
    }

    /// The window paints the same screen as wrapping the whole paragraph did,
    /// at every scroll position and two widths, cold and warm cache.
    #[test]
    fn window_matches_full_paragraph_at_every_scroll() {
        let h = 7u16;
        let mut checked = 0;
        for w in [23u16, 41] {
            let full = Paragraph::new(Text::from(sample())).wrap(Wrap { trim: false });
            let total = full.line_count(w) as u16;
            for _pass in 0..2 {
                for back in 0..=total {
                    let mut sb = back;
                    let win = window(sample(), w, h, &mut sb);
                    assert_eq!(win.total, u32::from(total));
                    let max_scroll = total.saturating_sub(h);
                    assert_eq!(sb, back.min(max_scroll));
                    let want = paint(sample(), max_scroll - sb, w, h);
                    assert_eq!(
                        paint(win.lines, win.scroll, w, h),
                        want,
                        "w={w} back={back}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 40, "only {checked} positions exercised");
    }

    /// A repaint that changes one line wraps only that line again.
    #[test]
    fn unchanged_lines_are_not_rewrapped() {
        let w = 29;
        let mut sb = 0;
        window(sample(), w, 7, &mut sb);
        let before = ROWS.with(|c| c.borrow().len());
        let mut next = sample();
        next[4] = Line::from("changed");
        window(next, w, 7, &mut sb);
        assert_eq!(ROWS.with(|c| c.borrow().len()), before + 1);
    }
}
