//! #1645: long mixed CJK/Latin prose wrapped at the last space (ratatui's
//! `Wrap` treats a run of CJK as one word), and continuation rows lost the
//! message indent and opened with the space the break landed on. Paints a
//! settled agent reply the way `render_transcript` does and checks every row.

use super::super::message::push_message;
use super::band_rows;
use crate::cmd::agent::cli::app::{ChatMsg, Role};
use crate::cmd::agent::cli::markdown::{self, BODY_INDENT};
use crate::cmd::agent::cli::theme::ANSI;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Text;
use ratatui::widgets::{Block, Padding, Paragraph, Widget, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const PROSE: &str = "搜尋結果裡的另外兩個（mur_native_tools.yaml 和 2026-06-02-prefer-mur-project-search… 那份 spec/plan）是比較早的版本，只分「語意搜尋 vs grep」兩層。現在 mur-native-tools 把符號和結構的查詢轉給 mur-search。\n\n順帶一提：我這個 session 的工具清單裡沒有 find_symbol，所以照技能的規則，我在符號層只能退回 rg -w。要讓 serena 生效，得把它的 MCP 接到 agent 上。";

/// Painted rows as (leading-blank columns, text) — wide glyphs counted once.
fn paint(width: u16) -> (Vec<(usize, String)>, u16) {
    let mut m = ChatMsg::for_test(Role::Agent, PROSE);
    m.streaming = false;
    m.rendered =
        Some(markdown::render(PROSE, markdown::body_cols(width, ANSI.inner_padding), &ANSI).lines);
    let mut lines = Vec::new();
    push_message(&mut lines, &m, 0, &ANSI, false, width);
    let measured = band_rows(&ANSI, lines.clone(), width);
    let area = Rect::new(0, 0, width, 80);
    let mut buf = Buffer::empty(area);
    Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .block(Block::default().padding(Padding::horizontal(ANSI.inner_padding as u16)))
        .render(area, &mut buf);
    let w = usize::from(width);
    let mut rows = Vec::new();
    for row in buf.content.chunks(w) {
        let mut l = String::new();
        let mut i = 0;
        while i < row.len() {
            let sym = row[i].symbol();
            l.push_str(sym);
            i += UnicodeWidthStr::width(sym).max(1);
        }
        let lead = l.len() - l.trim_start().len();
        rows.push((lead, l.trim().to_string()));
    }
    let painted = rows
        .iter()
        .rposition(|(_, t)| !t.is_empty())
        .map_or(0, |i| i + 1) as u16;
    rows.truncate(usize::from(painted));
    (rows, measured)
}

#[test]
fn mixed_cjk_prose_wraps_at_the_edge_under_the_indent() {
    let pad = usize::from(ANSI.inner_padding);
    let indent = pad + BODY_INDENT.len();
    for width in [40u16, 52, 61, 80, 92, 99, 113, 120] {
        let (rows, measured) = paint(width);
        let room = usize::from(width) - 2 * pad - BODY_INDENT.len();
        // rows[0] is the "● agent" header.
        let body = &rows[1..];
        for (i, (lead, text)) in body.iter().enumerate() {
            if text.is_empty() {
                continue;
            }
            assert_eq!(
                *lead, indent,
                "width={width} row {i} not under the indent: {text:?}"
            );
            // Premature break: the next row opens with a CJK glyph that would
            // still have fitted on this one.
            if let Some((_, next)) = body.get(i + 1)
                && let Some(c) = next.chars().next()
                && c.width() == Some(2)
            {
                let used = UnicodeWidthStr::width(text.as_str());
                // A glyph followed by closing punctuation moves down with it,
                // so no row opens on `，` (line-start prohibition).
                let carried: usize = next
                    .chars()
                    .take(2)
                    .enumerate()
                    .filter(|(k, ch)| *k == 0 || "，。、；：！？）」』】》…".contains(*ch))
                    .map(|(_, ch)| ch.width().unwrap_or(0))
                    .sum();
                assert!(
                    used + carried > room,
                    "width={width} row {i} broke early ({used}/{room}): {text:?} | {next:?}"
                );
            }
        }
        assert_eq!(
            measured as usize,
            rows.len(),
            "width={width} measured != painted"
        );
    }
}
