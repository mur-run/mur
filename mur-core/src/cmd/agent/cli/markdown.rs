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

const HEADING: Color = Color::Cyan;
const CODE: Color = Color::Yellow;
const QUOTE: Color = Color::DarkGray;
const RULE: &str = "────────────────────────";
const INDENT: &str = "  ";

/// Render Markdown source into owned ratatui `Text`.
pub fn render(src: &str) -> Text<'static> {
    let mut r = Renderer::default();
    let parser = Parser::new_ext(src, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES);
    for ev in parser {
        r.event(ev);
    }
    r.finish()
}

#[derive(Default)]
struct Renderer {
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
    /// `table_rows`, and `TagEnd::Table` renders them as aligned columns.
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

    /// Emit the collected rows as aligned, wrapped-free column lines: header
    /// row bold with a dim separator under it. Cell text keeps its inline
    /// styles (code, bold); widths are display-columns (CJK-safe), each
    /// column capped so a pathological cell cannot balloon the line.
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
        const CELL_MAX: usize = 24;
        let rows: Vec<Vec<Line<'static>>> = rows
            .into_iter()
            .map(|mut r| {
                while r.len() < ncols {
                    r.push(Vec::new());
                }
                r.into_iter()
                    .map(|spans| {
                        Line::from(
                            spans
                                .into_iter()
                                .map(|s| {
                                    // Guard against stray newlines inside a cell.
                                    Span::styled(s.content.replace('\n', " "), s.style)
                                })
                                .collect::<Vec<_>>(),
                        )
                    })
                    .collect()
            })
            .collect();
        let mut widths = vec![0usize; ncols];
        for row in &rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(cell.width());
            }
        }
        for w in &mut widths {
            *w = (*w).min(CELL_MAX);
        }
        for (ri, row) in rows.iter().enumerate() {
            let mut line: Vec<Span<'static>> = Vec::new();
            for (ci, cell) in row.iter().enumerate() {
                if ci > 0 {
                    line.push(Span::raw(" │ ".to_string()));
                }
                let pad = " ".repeat(widths[ci].saturating_sub(cell.width()));
                line.push(Span::raw(pad));
                if ri == 0 {
                    // Header: bold, matching the heading style.
                    line.extend(
                        cell.spans
                            .iter()
                            .cloned()
                            .map(|s| Span::styled(s.content, s.style.add_modifier(Modifier::BOLD))),
                    );
                } else {
                    line.extend(cell.spans.iter().cloned());
                }
            }
            self.lines.push(Line::from(line));
            if ri == 0 {
                let sep = widths
                    .iter()
                    .map(|w| "─".repeat(*w))
                    .collect::<Vec<_>>()
                    .join("─┼─");
                self.lines
                    .push(Line::styled(sep, Style::default().fg(QUOTE)));
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

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
        // Header and both body rows survive as their own lines with the column
        // separator; the markdown pipes themselves are gone.
        assert!(lines[0].contains("file:line"), "lines: {lines:?}");
        assert!(lines[0].contains("│"));
        assert!(lines[2].contains("CONFIRMED"));
        assert!(lines[2].contains("│"));
        assert!(lines[3].contains("REJECTED"));
        // Header row cell spans are styled bold (pad/separator spans aren't).
        assert!(
            t.lines[0]
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD))
        );
        // A separator line sits under the header.
        assert!(lines[1].contains('─'));
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
