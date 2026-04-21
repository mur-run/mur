//! Markdown heading-aware chunker.
//!
//! Splits a document into chunks by H1/H2/H3 boundaries and, within a chunk,
//! by paragraph boundaries if the byte budget is exceeded. Retains heading
//! path (list of current headings) for provenance.

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

/// A chunk of markdown text with provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkdownChunk {
    /// Hierarchy of headings at the chunk's start (e.g. ["Design", "Error handling"]).
    pub heading_path: Vec<String>,
    /// Inclusive character offsets in the ORIGINAL body.
    pub char_range: (usize, usize),
    /// Plaintext content (markdown syntax stripped to embedding-friendly form).
    pub text: String,
}

/// Chunk a markdown body.
///
/// - `title` prepends the chunker's notion of a "document title" into the heading
///   path of every chunk (so searches over titles + sections work).
/// - `max_chars` is a soft byte budget: chunks exceeding it are split at the
///   nearest paragraph boundary.
pub fn chunk_markdown(title: &str, body: &str, max_chars: usize) -> Vec<MarkdownChunk> {
    let mut out: Vec<MarkdownChunk> = Vec::new();
    let mut heading_stack: Vec<(u8, String)> = Vec::new(); // (level, heading text)
    let mut cur_buf = String::new();
    let mut cur_start: usize = 0;
    let mut in_heading: Option<HeadingLevel> = None;
    let mut heading_text_buf = String::new();

    let flush = |heading_stack: &Vec<(u8, String)>,
                 cur_buf: &mut String,
                 cur_start: &mut usize,
                 next_start: usize,
                 out: &mut Vec<MarkdownChunk>| {
        let text = std::mem::take(cur_buf).trim().to_string();
        if !text.is_empty() {
            let hp: Vec<String> = Some(title.to_string())
                .into_iter()
                .chain(heading_stack.iter().map(|(_, h)| h.clone()))
                .filter(|s| !s.is_empty())
                .collect();
            out.push(MarkdownChunk {
                heading_path: hp,
                char_range: (*cur_start, next_start),
                text,
            });
        }
        *cur_start = next_start;
    };

    // pulldown-cmark offset iterator yields (Event, byte-range) tuples.
    let offset_iter = Parser::new(body).into_offset_iter();

    for (event, range) in offset_iter {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let next_start = range.start;
                flush(&heading_stack, &mut cur_buf, &mut cur_start, next_start, &mut out);
                in_heading = Some(level);
                heading_text_buf.clear();
            }
            Event::End(TagEnd::Heading(level)) => {
                let text = std::mem::take(&mut heading_text_buf).trim().to_string();
                let depth = heading_level_to_u8(level);
                while let Some(&(d, _)) = heading_stack.last() {
                    if d >= depth {
                        heading_stack.pop();
                    } else {
                        break;
                    }
                }
                if !text.is_empty() {
                    heading_stack.push((depth, text));
                }
                in_heading = None;
                cur_start = range.end;
            }
            Event::Text(t) => {
                if in_heading.is_some() {
                    heading_text_buf.push_str(&t);
                } else {
                    cur_buf.push_str(&t);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if in_heading.is_none() {
                    cur_buf.push('\n');
                }
            }
            Event::End(TagEnd::Paragraph) => {
                cur_buf.push_str("\n\n");
                if cur_buf.len() > max_chars {
                    flush(
                        &heading_stack,
                        &mut cur_buf,
                        &mut cur_start,
                        range.end,
                        &mut out,
                    );
                }
            }
            Event::Code(c) => {
                if in_heading.is_none() {
                    cur_buf.push_str(&format!("`{c}`"));
                }
            }
            Event::Start(Tag::CodeBlock(_)) => {
                cur_buf.push_str("\n```\n");
            }
            Event::End(TagEnd::CodeBlock) => {
                cur_buf.push_str("\n```\n");
            }
            _ => {}
        }
    }

    flush(
        &heading_stack,
        &mut cur_buf,
        &mut cur_start,
        body.len(),
        &mut out,
    );

    out
}

fn heading_level_to_u8(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_body_returns_no_chunks() {
        let chunks = chunk_markdown("T", "", 1000);
        assert!(chunks.is_empty());
    }

    #[test]
    fn single_paragraph_single_chunk() {
        let chunks = chunk_markdown("T", "Hello world.", 1000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].heading_path, vec!["T".to_string()]);
        assert!(chunks[0].text.contains("Hello world"));
    }

    #[test]
    fn h1_h2_chunks_track_heading_path() {
        let body = "# Design\n\nintro para\n\n## Error handling\n\nsecond para\n";
        let chunks = chunk_markdown("Doc", body, 1000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading_path, vec!["Doc".to_string(), "Design".to_string()]);
        assert!(chunks[0].text.contains("intro para"));
        assert_eq!(
            chunks[1].heading_path,
            vec![
                "Doc".to_string(),
                "Design".to_string(),
                "Error handling".to_string()
            ]
        );
        assert!(chunks[1].text.contains("second para"));
    }

    #[test]
    fn oversized_chunk_splits_on_paragraph() {
        let big = "x".repeat(200);
        let body = format!("{big}\n\n{big}\n\n{big}");
        let chunks = chunk_markdown("T", &body, 150);
        assert!(chunks.len() >= 3);
    }

    #[test]
    fn sibling_h2_does_not_leak_previous_h2() {
        let body = "## A\n\npara A\n\n## B\n\npara B\n";
        let chunks = chunk_markdown("Doc", body, 1000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(
            chunks[0].heading_path,
            vec!["Doc".to_string(), "A".to_string()]
        );
        assert_eq!(
            chunks[1].heading_path,
            vec!["Doc".to_string(), "B".to_string()]
        );
    }

    #[test]
    fn char_range_covers_body() {
        let body = "one\n\ntwo";
        let chunks = chunk_markdown("T", body, 1000);
        let last = chunks.last().unwrap();
        assert_eq!(last.char_range.1, body.len());
    }
}
