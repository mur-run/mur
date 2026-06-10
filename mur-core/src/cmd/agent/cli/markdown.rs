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
        self.cur.push(Span::styled(text.to_string(), style));
    }

    /// Push the in-progress spans as a line. A no-op when there is nothing
    /// buffered — blank/spacer lines are added only via [`blank_line`], so a
    /// `flush_line` between two list items never injects a stray blank (which
    /// previously rendered tight lists double-spaced with a leading blank).
    fn flush_line(&mut self) {
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
                self.cur
                    .push(Span::styled(t.to_string(), Style::default().fg(CODE)));
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
                if self.list_stack.is_empty() {
                    self.flush_line();
                }
                self.indent();
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
                self.flush_line();
                if self.list_stack.is_empty() {
                    self.blank_line();
                }
            }
            _ => {}
        }
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
