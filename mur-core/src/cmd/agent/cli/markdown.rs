//! Minimal Markdown → ratatui `Text` renderer.
//!
//! Completed agent replies are re-rendered through this so the TUI shows
//! headings, bold/italic, lists, blockquotes, and fenced code instead of raw
//! Markdown. It reuses the workspace's existing `pulldown-cmark` dependency, so
//! no new crate is pulled in. It is deliberately small: it targets the subset of
//! Markdown an assistant actually emits, not full CommonMark fidelity.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use unicode_width::UnicodeWidthChar;

const HEADING: Color = Color::Cyan;
const CODE: Color = Color::Yellow;
const QUOTE: Color = Color::DarkGray;
const RULE: &str = "────────────────────────";
const INDENT: &str = "  ";

/// Body indent under a role header ("you ›" / "● agent") so a message's
/// content reads as belonging to its speaker rather than sitting flush with
/// the header. Owned here because it is part of the width a body may use.
pub(crate) const BODY_INDENT: &str = "  ";

/// Never lay a table out narrower than this, whatever the pane says: below it
/// every cell is a ladder of single characters and nothing is readable.
const MIN_BODY_COLS: usize = 20;

/// Narrowest a table column is ever squeezed to.
const MIN_COL: usize = 4;

/// Columns a message body may use inside a pane `pane_width` wide: the pane
/// minus its horizontal padding on both sides and the body indent.
pub(crate) fn body_cols(pane_width: u16, inner_padding: u8) -> usize {
    (pane_width as usize)
        .saturating_sub(2 * inner_padding as usize + BODY_INDENT.len())
        .max(MIN_BODY_COLS)
}

/// Render Markdown source into owned ratatui `Text`. `width` is the columns
/// the text will be painted into; prose wraps at paint time, but a table has
/// to know its width here to decide column widths and wrap its cells.
pub fn render(src: &str, width: usize) -> Text<'static> {
    let mut r = Renderer {
        width: width.max(MIN_BODY_COLS),
        ..Renderer::default()
    };
    let parser = Parser::new_ext(src, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES);
    for ev in parser {
        r.event(ev);
    }
    r.finish()
}

#[derive(Default)]
struct Renderer {
    /// Columns available to a table (see `render`).
    width: usize,
    lines: Vec<Line<'static>>,
    cur: Vec<Span<'static>>,
    bold: bool,
    italic: bool,
    code: bool,
    in_code_block: bool,
    quote: bool,
    /// One entry per open list; `Some(n)` = ordered list next number.
    list_stack: Vec<Option<u64>>,
    /// Table collection: while inside a table, text/code/emphasis accumulate
    /// into `table_cur_cell` instead of `cur`, rows are collected in
    /// `table_rows`, and `TagEnd::Table` renders them as a boxed table.
    in_table: bool,
    table_rows: Vec<Vec<Vec<Span<'static>>>>,
    table_cur_cell: Vec<Span<'static>>,
}

impl Renderer {
    fn span_style(&self) -> Style {
        let mut s = Style::default();
        if self.bold {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.italic {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if self.code {
            s = s.fg(CODE);
        }
        s
    }

    fn push_text(&mut self, text: &str) {
        let style = self.span_style();
        let span = Span::styled(text.to_string(), style);
        if self.in_table {
            self.table_cur_cell.push(span);
        } else {
            self.cur.push(span);
        }
    }

    /// Push the in-progress spans as a line. A no-op when there is nothing
    /// buffered — blank/spacer lines are added only via [`blank_line`], so a
    /// `flush_line` between two list items never injects a stray blank (which
    /// previously rendered tight lists double-spaced with a leading blank).
    fn flush_line(&mut self) {
        if self.in_table {
            return; // cell content stays in the cell buffer; lines come at table end
        }
        if self.cur.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.cur);
        self.lines.push(Line::from(spans));
    }

    fn blank_line(&mut self) {
        if !matches!(self.lines.last(), Some(l) if l.spans.is_empty()) {
            self.lines.push(Line::default());
        }
    }

    fn indent(&mut self) {
        let depth = self.list_stack.len().saturating_sub(1);
        if self.quote {
            self.cur
                .push(Span::styled("▏ ".to_string(), Style::default().fg(QUOTE)));
        }
        for _ in 0..depth {
            self.cur.push(Span::raw(INDENT.to_string()));
        }
    }

    fn event(&mut self, ev: Event) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                if self.in_code_block {
                    // Code text may contain newlines; render each as its own line.
                    for (i, part) in t.split('\n').enumerate() {
                        if i > 0 {
                            self.flush_line();
                        }
                        self.cur
                            .push(Span::styled(part.to_string(), Style::default().fg(CODE)));
                    }
                } else {
                    self.push_text(&t);
                }
            }
            Event::Code(t) => {
                let span = Span::styled(t.to_string(), Style::default().fg(CODE));
                if self.in_table {
                    self.table_cur_cell.push(span);
                } else {
                    self.cur.push(span);
                }
            }
            Event::SoftBreak => self.cur.push(Span::raw(" ".to_string())),
            Event::HardBreak => self.flush_line(),
            Event::Rule => {
                self.flush_line();
                self.lines
                    .push(Line::styled(RULE.to_string(), Style::default().fg(QUOTE)));
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Heading { .. } => {
                self.flush_line();
                self.bold = true;
            }
            Tag::Strong => self.bold = true,
            Tag::Emphasis => self.italic = true,
            Tag::CodeBlock(_) => {
                self.flush_line();
                self.in_code_block = true;
            }
            Tag::BlockQuote(_) => self.quote = true,
            Tag::List(start) => self.list_stack.push(start),
            Tag::Item => {
                self.flush_line();
                self.indent();
                let marker = match self.list_stack.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "• ".to_string(),
                };
                self.cur
                    .push(Span::styled(marker, Style::default().fg(HEADING)));
            }
            Tag::Paragraph => {
                if self.in_table {
                    return; // cells wrap their text in paragraphs; no indent/flush
                }
                if self.list_stack.is_empty() {
                    self.flush_line();
                }
                self.indent();
            }
            Tag::Table(_) => {
                self.flush_line();
                self.in_table = true;
                self.table_rows.clear();
                self.table_cur_cell.clear();
            }
            // The head row is wrapped in `TableHead`, not `TableRow` — both
            // must open a new row or the header cells are silently dropped.
            Tag::TableHead | Tag::TableRow => {
                // End any in-progress cell (malformed tables may omit the
                // close tag), then start a fresh row.
                if !self.table_cur_cell.is_empty()
                    && let Some(row) = self.table_rows.last_mut()
                {
                    row.push(std::mem::take(&mut self.table_cur_cell));
                }
                self.table_rows.push(Vec::new());
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(level) => {
                self.bold = false;
                self.flush_line();
                if matches!(level, HeadingLevel::H1 | HeadingLevel::H2) {
                    self.blank_line();
                }
            }
            TagEnd::Strong => self.bold = false,
            TagEnd::Emphasis => self.italic = false,
            TagEnd::CodeBlock => {
                self.flush_line();
                self.in_code_block = false;
                self.blank_line();
            }
            TagEnd::BlockQuote(_) => {
                self.quote = false;
                self.flush_line();
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.blank_line();
                }
            }
            TagEnd::Item => self.flush_line(),
            TagEnd::Paragraph => {
                if self.in_table {
                    return;
                }
                self.flush_line();
                if self.list_stack.is_empty() {
                    self.blank_line();
                }
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.table_cur_cell);
                if let Some(row) = self.table_rows.last_mut() {
                    row.push(cell);
                }
            }
            TagEnd::TableRow => {
                if !self.table_cur_cell.is_empty()
                    && let Some(row) = self.table_rows.last_mut()
                {
                    row.push(std::mem::take(&mut self.table_cur_cell));
                }
            }
            TagEnd::Table => self.render_table(),
            _ => {}
        }
    }

    /// Emit the collected rows as a boxed table: `┌─┬─┐` borders, header row
    /// bold with a `├─┼─┤` rule under it, one blank line after. Columns take
    /// their natural width when the table fits; otherwise they share the
    /// pane proportionally (never below `MIN_COL`) and cells wrap inside
    /// their column, so a wide table stays inside the terminal instead of
    /// spilling into the pane's own line-wrap and losing its grid. Widths are
    /// display columns (CJK-safe); cell text keeps its inline styles.
    fn render_table(&mut self) {
        let rows = std::mem::take(&mut self.table_rows);
        self.in_table = false;
        self.table_cur_cell.clear();
        if rows.is_empty() {
            return;
        }
        let ncols = rows.iter().map(Vec::len).max().unwrap_or(0);
        if ncols == 0 {
            return;
        }
        let rows: Vec<Vec<Vec<Span<'static>>>> = rows
            .into_iter()
            .map(|mut r| {
                while r.len() < ncols {
                    r.push(Vec::new());
                }
                r.into_iter()
                    .map(|spans| {
                        spans
                            .into_iter()
                            // Guard against stray newlines inside a cell.
                            .map(|s| Span::styled(s.content.replace('\n', " "), s.style))
                            .collect()
                    })
                    .collect()
            })
            .collect();
        let mut natural = vec![0usize; ncols];
        for row in &rows {
            for (i, cell) in row.iter().enumerate() {
                natural[i] = natural[i].max(Line::from(cell.clone()).width());
            }
        }
        // Every column costs its text plus one space each side and a border;
        // the last border closes the row.
        let room = self.width.saturating_sub(3 * ncols + 1);
        let widths = fit_columns(&natural, room);

        let rule = |l: &str, m: &str, r: &str| -> Line<'static> {
            let bars = widths
                .iter()
                .map(|w| "─".repeat(w + 2))
                .collect::<Vec<_>>()
                .join(m);
            Line::styled(format!("{l}{bars}{r}"), Style::default().fg(QUOTE))
        };
        let border = Style::default().fg(QUOTE);

        self.lines.push(rule("┌", "┬", "┐"));
        for (ri, row) in rows.iter().enumerate() {
            let cells: Vec<Vec<Line<'static>>> = row
                .iter()
                .zip(&widths)
                .map(|(spans, w)| wrap_spans(spans, *w))
                .collect();
            let height = cells.iter().map(Vec::len).max().unwrap_or(1);
            for k in 0..height {
                let mut line: Vec<Span<'static>> = vec![Span::styled("│ ", border)];
                for (ci, cell) in cells.iter().enumerate() {
                    if ci > 0 {
                        line.push(Span::styled(" │ ", border));
                    }
                    let (text, used) = match cell.get(k) {
                        Some(l) => (l.spans.clone(), l.width()),
                        None => (Vec::new(), 0),
                    };
                    if ri == 0 {
                        // Header: bold, matching the heading style.
                        line.extend(text.into_iter().map(|s| {
                            Span::styled(s.content, s.style.add_modifier(Modifier::BOLD))
                        }));
                    } else {
                        line.extend(text);
                    }
                    line.push(Span::raw(" ".repeat(widths[ci].saturating_sub(used))));
                }
                line.push(Span::styled(" │", border));
                self.lines.push(Line::from(line));
            }
            if ri == 0 {
                self.lines.push(rule("├", "┼", "┤"));
            }
        }
        self.lines.push(rule("└", "┴", "┘"));
        self.blank_line();
    }

    fn finish(mut self) -> Text<'static> {
        self.flush_line();
        // Trim a trailing blank line.
        while matches!(self.lines.last(), Some(l) if l.spans.is_empty()) {
            self.lines.pop();
        }
        Text::from(self.lines)
    }
}

/// Column widths for a table whose columns want `natural` widths inside
/// `room` columns of text. Fits as-is when it can. Otherwise the squeeze
/// comes off the widest columns first: find the largest cap such that every
/// column clipped to it fits, then hand any leftover columns back to the
/// clipped ones. A short `crate` column next to a long `description` column
/// keeps its width and only the description wraps — a proportional split
/// squeezed every column and wrapped `mur-agent-runtime` in two for no
/// gain. Never below `MIN_COL`; a pane too narrow even for that overflows
/// into the pane's own wrap rather than dropping columns.
fn fit_columns(natural: &[usize], room: usize) -> Vec<usize> {
    let clipped_sum = |cap: usize| natural.iter().map(|n| (*n).min(cap)).sum::<usize>();
    if clipped_sum(usize::MAX) <= room {
        return natural.to_vec();
    }
    let (mut lo, mut hi) = (MIN_COL, natural.iter().copied().max().unwrap_or(MIN_COL));
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if clipped_sum(mid) <= room {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    // `lo` is never below `MIN_COL`, so clipping is the only floor needed;
    // a column narrower than the floor keeps its own width.
    let mut widths: Vec<usize> = natural.iter().map(|n| (*n).min(lo)).collect();
    let mut left = room.saturating_sub(widths.iter().sum::<usize>());
    for (i, n) in natural.iter().enumerate() {
        if left == 0 {
            break;
        }
        if *n > widths[i] {
            widths[i] += 1;
            left -= 1;
        }
    }
    widths
}

/// Greedy wrap of styled text to `w` display columns. Breaks at the last
/// space that falls inside the line when there is one, otherwise between
/// characters (CJK text has no spaces to break at); a character wider than
/// the column is emitted on its own line rather than dropped. Span styles
/// survive the split. Always yields at least one line.
fn wrap_spans(spans: &[Span<'static>], w: usize) -> Vec<Line<'static>> {
    let w = w.max(1);
    let chars: Vec<(char, Style)> = spans
        .iter()
        .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
        .collect();
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        if start > 0 {
            while start < chars.len() && chars[start].0 == ' ' {
                start += 1;
            }
            if start >= chars.len() {
                break;
            }
        }
        let mut end = start;
        let mut used = 0;
        let mut last_space = None;
        while end < chars.len() {
            let cw = chars[end].0.width().unwrap_or(0);
            if used + cw > w {
                break;
            }
            if chars[end].0 == ' ' {
                last_space = Some(end);
            }
            used += cw;
            end += 1;
        }
        if end < chars.len()
            && let Some(sp) = last_space
            && sp > start
        {
            end = sp;
        }
        if end == start {
            end = start + 1;
        }
        out.push(line_from_chars(&chars[start..end]));
        start = end;
    }
    if out.is_empty() {
        out.push(Line::default());
    }
    out
}

/// Rebuild spans from a run of styled characters, merging neighbours that
/// share a style.
fn line_from_chars(chars: &[(char, Style)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (c, style) in chars {
        match spans.last_mut() {
            Some(s) if s.style == *style => s.content.to_mut().push(*c),
            _ => spans.push(Span::styled(c.to_string(), *style)),
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    /// Every test renders into a plain 80-column body unless it says otherwise.
    fn render(src: &str) -> Text<'static> {
        super::render(src, 80)
    }

    fn plain(text: &Text) -> String {
        text.lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_heading_and_paragraph() {
        let t = render("# Title\n\nHello world");
        let s = plain(&t);
        assert!(s.contains("Title"));
        assert!(s.contains("Hello world"));
    }

    #[test]
    fn renders_bullets_and_ordered() {
        let t = render("- a\n- b\n\n1. one\n2. two");
        let s = plain(&t);
        assert!(s.contains("• a"));
        assert!(s.contains("• b"));
        assert!(s.contains("1. one"));
        assert!(s.contains("2. two"));
    }

    #[test]
    fn renders_table_with_aligned_columns_and_bold_header() {
        let t = render(
            "| file:line | Status | Evidence |\n\
             | --- | --- | --- |\n\
             | `murmurd.rs:130` | CONFIRMED | lock read fail-open |\n\
             | `step.rs:56` | REJECTED | — |",
        );
        let lines = t
            .lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        // Boxed: top rule, header, rule, body rows, bottom rule. The markdown
        // pipes themselves are gone.
        assert!(
            lines[0].starts_with('┌') && lines[0].ends_with('┐'),
            "lines: {lines:?}"
        );
        assert!(lines[1].contains("file:line"));
        assert!(lines[1].contains("│"));
        assert!(lines[2].starts_with('├') && lines[2].contains('┼'));
        assert!(lines[3].contains("CONFIRMED"));
        assert!(lines[4].contains("REJECTED"));
        assert!(lines[5].starts_with('└') && lines[5].ends_with('┘'));
        // Header row cell spans are styled bold (pad/separator spans aren't).
        assert!(
            t.lines[1]
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD))
        );
        // Every row of the grid is the same display width.
        let widths: Vec<usize> = lines.iter().map(|l| l.width()).collect();
        assert!(
            widths.iter().all(|w| *w == widths[0]),
            "ragged grid: {widths:?}"
        );
    }

    /// A table wider than the pane used to spill into the pane's own wrap,
    /// which breaks rows mid-cell and loses the grid entirely. Now the
    /// columns share the width and the cells wrap inside them.
    #[test]
    fn a_wide_table_wraps_its_cells_inside_the_width() {
        let long = "one two three four five six seven eight nine ten eleven twelve";
        let src = format!("| key | value |\n| --- | --- |\n| k | {long} |");
        let t = super::render(&src, 40);
        let lines = rows(&t);
        assert!(lines.iter().all(|l| l.width() <= 40), "overflow: {lines:?}");
        let body: Vec<&String> = lines.iter().filter(|l| l.starts_with('│')).collect();
        assert!(body.len() > 2, "long cell did not wrap: {lines:?}");
        // Nothing lost in the wrap: every word is still there, in order.
        let joined: String = lines.join(" ");
        let mut pos = 0;
        for word in long.split(' ') {
            let at = joined[pos..]
                .find(word)
                .unwrap_or_else(|| panic!("lost {word}"));
            pos += at + word.len();
        }
        assert!(lines.last().unwrap().starts_with('└'));
    }

    /// The squeeze comes off the widest column: short columns keep their
    /// natural width when clipping the long one alone makes the table fit.
    #[test]
    fn narrow_columns_survive_when_the_wide_one_can_absorb_the_squeeze() {
        assert_eq!(fit_columns(&[17, 80, 18, 28], 93), vec![17, 30, 18, 28]);
        // Two wide columns share the clip; leftover goes back one column at a time.
        assert_eq!(fit_columns(&[5, 40, 40], 60), vec![5, 28, 27]);
        // Fits: untouched.
        assert_eq!(fit_columns(&[5, 10], 40), vec![5, 10]);
    }

    /// Wide (CJK) characters are two columns each; the grid must line up on
    /// display width, not character count.
    #[test]
    fn cjk_cells_keep_the_grid_aligned() {
        let t = render("| 欄位 | 值 |\n| --- | --- |\n| 訊息通道 | ab |\n| x | 憑證 |");
        let lines = rows(&t);
        let widths: Vec<usize> = lines.iter().map(|l| l.width()).collect();
        assert!(
            widths.iter().all(|w| *w == widths[0]),
            "ragged grid: {widths:?}"
        );
        // and a CJK cell wraps between characters when squeezed
        let t = super::render(
            "| a | b |\n| --- | --- |\n| 這是一段很長的中文內容用來測試換行 | y |",
            24,
        );
        let lines = rows(&t);
        assert!(lines.iter().all(|l| l.width() <= 24), "overflow: {lines:?}");
        assert!(lines.iter().filter(|l| l.starts_with('│')).count() > 2);
    }

    #[test]
    fn table_does_not_bleed_into_following_paragraph() {
        let t = render("| a | b |\n| --- | --- |\n| 1 | 2 |\n\nAfter the table.");
        let s = plain(&t);
        assert!(s.contains("After the table."));
        assert!(s.contains("│"));
    }

    fn rows(text: &Text) -> Vec<String> {
        text.lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn tight_list_has_no_blank_or_phantom_lines() {
        // Regression: items used to render double-spaced with a leading blank.
        assert_eq!(rows(&render("- a\n- b")), vec!["• a", "• b"]);
        assert_eq!(rows(&render("1. one\n2. two")), vec!["1. one", "2. two"]);
    }

    #[test]
    fn renders_code_block_contents() {
        let t = render("```\nlet x = 1;\n```");
        assert!(plain(&t).contains("let x = 1;"));
    }

    #[test]
    fn inline_code_and_bold_do_not_panic() {
        let t = render("This is `code` and **bold** and *italic*.");
        let s = plain(&t);
        assert!(s.contains("code"));
        assert!(s.contains("bold"));
        assert!(s.contains("italic"));
    }
}
