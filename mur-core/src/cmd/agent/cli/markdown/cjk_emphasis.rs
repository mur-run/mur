//! Bold that CommonMark leaves literal next to CJK punctuation.
//!
//! `。**33f30194 這次已確認。**上一輪` shows its `**`: a closing `**` preceded
//! by CJK punctuation and followed by a CJK letter is not right-flanking, so
//! pulldown-cmark 0.12 emits the delimiters as plain `Text("*")` events. This
//! pass runs over the parser's events, joins each run of adjacent `Text`
//! events, and re-splits a `**…**` pair the parser left literal into
//! `Strong` — only when the pair touches a wide (CJK) character, so ordinary
//! CommonMark, which the parser already resolved, is never touched.

use pulldown_cmark::{CowStr, Event, Tag, TagEnd};
use unicode_width::UnicodeWidthChar;

const DELIM: &str = "**";

fn wide(c: Option<char>) -> bool {
    c.is_some_and(|c| c.width().unwrap_or(0) >= 2)
}

/// Rewrite `events`, turning literal CJK-adjacent `**…**` pairs into bold.
pub(super) fn fix<'a>(events: impl Iterator<Item = Event<'a>>) -> Vec<Event<'a>> {
    let mut out = Vec::new();
    let mut run = String::new();
    let mut in_code_block = false;
    for ev in events {
        match ev {
            Event::Text(t) if !in_code_block => run.push_str(&t),
            other => {
                flush(&mut run, &mut out);
                match &other {
                    Event::Start(Tag::CodeBlock(_)) => in_code_block = true,
                    Event::End(TagEnd::CodeBlock) => in_code_block = false,
                    _ => {}
                }
                out.push(other);
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

fn flush<'a>(run: &mut String, out: &mut Vec<Event<'a>>) {
    if run.is_empty() {
        return;
    }
    let text = std::mem::take(run);
    let mut rest = text.as_str();
    while let Some((before, inner, after)) = next_pair(rest) {
        if !before.is_empty() {
            out.push(Event::Text(CowStr::from(before.to_string())));
        }
        out.push(Event::Start(Tag::Strong));
        out.push(Event::Text(CowStr::from(inner.to_string())));
        out.push(Event::End(TagEnd::Strong));
        rest = after;
    }
    if !rest.is_empty() {
        out.push(Event::Text(CowStr::from(rest.to_string())));
    }
}

/// The first `**inner**` in `s` that touches a CJK character on either
/// delimiter, as `(before, inner, after)`. `inner` is non-empty and does not
/// start or end with whitespace, as CommonMark requires of emphasis content.
fn next_pair(s: &str) -> Option<(&str, &str, &str)> {
    let mut from = 0;
    while let Some(o) = s[from..].find(DELIM).map(|i| i + from) {
        let body = o + DELIM.len();
        let c = s[body..].find(DELIM).map(|i| i + body)?;
        let inner = &s[body..c];
        let after = c + DELIM.len();
        let edge_ok = !inner.is_empty()
            && !inner.starts_with(char::is_whitespace)
            && !inner.ends_with(char::is_whitespace)
            && !inner.contains(DELIM);
        let touches_cjk = wide(s[..o].chars().next_back())
            || wide(inner.chars().next())
            || wide(inner.chars().next_back())
            || wide(s[after..].chars().next());
        if edge_ok && touches_cjk {
            return Some((&s[..o], inner, &s[after..]));
        }
        from = body;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::fix;
    use pulldown_cmark::{Event, Parser, Tag};

    fn strong_texts(src: &str) -> (Vec<String>, String) {
        let evs = fix(Parser::new(src));
        let mut bold = Vec::new();
        let mut all = String::new();
        let mut in_strong = false;
        for e in evs {
            match e {
                Event::Start(Tag::Strong) => in_strong = true,
                Event::End(_) => in_strong = false,
                Event::Text(t) => {
                    if in_strong {
                        bold.push(t.to_string());
                    }
                    all.push_str(&t);
                }
                _ => {}
            }
        }
        (bold, all)
    }

    #[test]
    fn cjk_punctuation_pair_becomes_bold() {
        let (bold, all) = strong_texts("上一輪說明。**33f30194 這次已確認。**上一輪");
        assert_eq!(bold, vec!["33f30194 這次已確認。".to_string()]);
        assert!(!all.contains("**"), "{all}");
    }

    #[test]
    fn ordinary_commonmark_is_untouched() {
        // `** b **` is not flanking, so CommonMark leaves it literal; with no
        // CJK beside it this pass must too.
        let (bold, all) = strong_texts("a **b** c and x ** y ** z");
        assert_eq!(bold, vec!["b".to_string()]);
        assert!(all.contains("x ** y ** z"), "ascii literal stays: {all}");
    }

    #[test]
    fn code_blocks_keep_their_asterisks() {
        let (bold, _) = strong_texts("```\n確認。**x**上\n```");
        assert!(bold.is_empty());
    }
}
