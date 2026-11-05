//! Renderer tests, split out of `markdown.rs` to keep it under 800 lines.

use super::*;
use unicode_width::UnicodeWidthStr;

/// Every test renders into a plain 80-column body unless it says otherwise.
fn render(src: &str) -> Text<'static> {
    super::render(src, 80, &crate::cmd::agent::cli::theme::ANSI)
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
        lines[0].starts_with('╭') && lines[0].ends_with('╮'),
        "lines: {lines:?}"
    );
    assert!(lines[1].contains("file:line"));
    assert!(lines[1].contains("│"));
    assert!(lines[2].starts_with('├') && lines[2].contains('┼'));
    assert!(lines[3].contains("CONFIRMED"));
    assert!(lines[4].contains("REJECTED"));
    assert!(lines[5].starts_with('╰') && lines[5].ends_with('╯'));
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
    let t = super::render(&src, 40, &crate::cmd::agent::cli::theme::ANSI);
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
    assert!(lines.last().unwrap().starts_with('╰'));
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
        &crate::cmd::agent::cli::theme::ANSI,
    );
    let lines = rows(&t);
    assert!(lines.iter().all(|l| l.width() <= 24), "overflow: {lines:?}");
    assert!(lines.iter().filter(|l| l.starts_with('│')).count() > 2);
}

/// A 40-column pane leaves 36 body columns; four columns at full cell
/// padding cost 21 of them, leaving less than `MIN_COL` each. The padding
/// gives way before the grid spills past the width.
#[test]
fn a_four_column_table_fits_a_narrow_pane() {
    let t = super::render(
        "| 級別 | 進入條件 | 工具 | 退路 |\n| --- | --- | --- | --- |\n| Exact | 錯誤訊息 | rg -n | 零結果本身就是答案 |",
        36,
        &crate::cmd::agent::cli::theme::ANSI,
    );
    let lines = rows(&t);
    assert!(lines.iter().all(|l| l.width() <= 36), "overflow: {lines:?}");
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

/// The fence's trailing newline used to paint a blank that `blank_line`
/// then doubled: two empty rows under every code block.
#[test]
fn a_code_block_is_followed_by_exactly_one_blank_row() {
    let r = rows(&render("before\n\n```\nlet x = 1;\n```\n\nafter"));
    assert_eq!(r, vec!["before", "", "let x = 1;", "", "after"]);
}

/// A newline the author typed inside a paragraph stays a newline.
#[test]
fn a_soft_break_starts_a_new_line() {
    assert_eq!(
        rows(&render("憑據：\n• x\n• y")),
        vec!["憑據：", "• x", "• y"]
    );
}

/// Two spaces between a cell's text and its border, each side.
#[test]
fn table_cells_are_padded() {
    let r = rows(&render("| a | b |\n| --- | --- |\n| 1 | 2 |"));
    assert_eq!(r[1], "│  a  │  b  │", "header: {r:?}");
    assert_eq!(r[3], "│  1  │  2  │", "body: {r:?}");
}

#[test]
fn inline_code_and_bold_do_not_panic() {
    let t = render("This is `code` and **bold** and *italic*.");
    let s = plain(&t);
    assert!(s.contains("code"));
    assert!(s.contains("bold"));
    assert!(s.contains("italic"));
}

/// Spec Problem 3: bold against CJK punctuation renders bold, no `**`.
#[test]
fn cjk_bold() {
    let t = render("上一輪說明。**33f30194 這次已確認。**上一輪");
    let line = &t.lines[0];
    assert!(!plain(&t).contains("**"), "{}", plain(&t));
    let bold: String = line
        .spans
        .iter()
        .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(bold, "33f30194 這次已確認。");
}
