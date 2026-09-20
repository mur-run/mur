//! Render an edit-tool's `{file_path, old_string, new_string}` args as a
//! `-`/`+`/context diff, using the `diff` crate.
//!
//! Consumed by `render_card::card_lines` to show an inline diff for edit-tool cards.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::theme::Theme;

/// Tool names (case-insensitive) whose args describe a file edit.
///
/// Two families, and the second is the one that was missing. `edit` / `write`
/// / `multiedit` / `str_replace*` are the spellings a Claude-CLI backend uses.
/// `edit_file` / `write_file` are the names MUR's OWN runtime registers
/// (`mur-agent-runtime/src/tools/{edit_file,write_file}.rs`), so a first-party
/// edit matched nothing here and fell through to a raw JSON args dump —
/// `new_string` printed as one escaped `\n`-laden line, which is the least
/// readable form of the thing the card exists to show.
fn is_edit_tool(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "edit"
            | "write"
            | "multiedit"
            | "str_replace"
            | "str_replace_editor"
            | "edit_file"
            | "write_file"
    )
}

fn str_field<'a>(args: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| args.get(*k).and_then(|v| v.as_str()))
}

/// One rendered diff row, before any styling or line cap is applied.
///
/// The TUI card and the Ctrl+O dump want the same *content* but disagree on
/// everything else: the card is styled ratatui spans capped at
/// [`DIFF_MAX_LINES`], the dump is unstyled plain text with no cap at all
/// (that's the whole point of the dump). Computing the rows once here is what
/// keeps the two renderers from drifting apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffPart {
    /// The file path header.
    Path(String),
    /// A removed line (`-`).
    Del(String),
    /// An added line (`+`).
    Add(String),
    /// An unchanged context line.
    Ctx(String),
}

/// Compute the diff rows for an edit-like tool call, or `None` if this isn't
/// an edit we can render. Uncapped and unstyled — callers apply their own.
pub fn edit_diff_parts(name: &str, args: &serde_json::Value) -> Option<Vec<DiffPart>> {
    if !is_edit_tool(name) {
        return None;
    }
    let path = str_field(args, &["file_path", "path"]).unwrap_or("");
    let old = str_field(args, &["old_string", "old_str"]);
    // Need at least the `new` side to render anything.
    let new = str_field(args, &["new_string", "new_str", "content"])?;

    let mut parts: Vec<DiffPart> = Vec::new();
    if !path.is_empty() {
        parts.push(DiffPart::Path(path.to_string()));
    }

    match old {
        Some(old_str) => {
            // Full diff: context lines (both), removed (Left), added (Right).
            for hunk in diff::lines(old_str, new) {
                parts.push(match hunk {
                    diff::Result::Left(l) => DiffPart::Del(l.to_string()),
                    diff::Result::Right(r) => DiffPart::Add(r.to_string()),
                    diff::Result::Both(l, _) => DiffPart::Ctx(l.to_string()),
                });
            }
        }
        None => {
            // No old string: just show the new content as additions.
            for line in new.lines() {
                parts.push(DiffPart::Add(line.to_string()));
            }
        }
    }

    Some(parts)
}

/// The diff rows as plain, unstyled, **uncapped** text lines — the form the
/// Ctrl+O scrollback dump wants. `None` when this isn't an edit tool.
pub fn edit_diff_text(name: &str, args: &serde_json::Value) -> Option<Vec<String>> {
    Some(
        edit_diff_parts(name, args)?
            .into_iter()
            .map(|p| match p {
                DiffPart::Path(p) => format!(" {p}"),
                DiffPart::Del(l) => format!("  - {l}"),
                DiffPart::Add(l) => format!("  + {l}"),
                DiffPart::Ctx(l) => format!("    {l}"),
            })
            .collect(),
    )
}

/// Build styled `-`/`+` diff lines for an edit-like tool call, or `None` if
/// this isn't an edit we can render. The transcript viewport provides scrolling;
/// keeping all rows here means expanded cards never silently hide a file change.
///
/// Rows are TWO spans, not one styled line: a coloured `▌-` / `▌+` gutter and
/// then the code in ordinary text colour. Painting the whole row red or green
/// (what this used to do) makes a diff read like a stack of error and success
/// banners, and it fights every syntax colour the content already carries —
/// the sign belongs in the gutter, the way a code review shows it.
pub fn edit_diff_lines(
    name: &str,
    args: &serde_json::Value,
    theme: &'static Theme,
) -> Option<Vec<Line<'static>>> {
    let parts = edit_diff_parts(name, args)?;

    Some(
        parts
            .into_iter()
            .map(|part| match part {
                DiffPart::Path(p) => {
                    Line::styled(format!(" {p}"), theme.muted.add_modifier(Modifier::BOLD))
                }
                DiffPart::Del(l) => Line::from(vec![
                    Span::styled("  ▌- ", theme.diff_del_mark.patch(theme.diff_del_bg)),
                    Span::styled(l, theme.diff_del_text.patch(theme.diff_del_bg)),
                ])
                .style(theme.diff_del_bg),
                DiffPart::Add(l) => Line::from(vec![
                    Span::styled("  ▌+ ", theme.diff_add_mark.patch(theme.diff_add_bg)),
                    Span::styled(l, theme.diff_add_text.patch(theme.diff_add_bg)),
                ])
                .style(theme.diff_add_bg),
                DiffPart::Ctx(l) => Line::from(vec![
                    Span::styled("  │ ", theme.muted.add_modifier(Modifier::DIM)),
                    Span::styled(l, theme.muted.add_modifier(Modifier::DIM)),
                ]),
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::edit_diff_lines;
    use crate::cmd::agent::cli::theme;

    fn text(lines: &[ratatui::text::Line]) -> String {
        // Join spans WITHIN a row, rows with newlines. A diff row is a
        // coloured gutter span plus a text span, so flattening every span
        // with `\n` would split `▌+ ` from the code it marks.
        lines
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

    /// The colour lives in the gutter, NOT on the code. A reviewer reads the
    /// text of a diff; painting every changed row red or green turns an edit
    /// into two blocks of alarm colour and overrides any styling the content
    /// carries. Guard both halves: the mark span is coloured, the code span
    /// is the theme's ordinary text style.
    ///
    /// The sign never gets painted across the code on a skin that can tint the
    /// row instead — a red line and a green line read as banners. `ansi` is
    /// the exception and the reason the rule is per-skin: with no background
    /// tint available it has nowhere but the ink to put the signal.
    #[test]
    fn diff_rows_put_the_change_in_the_gutter_or_the_ink_never_both() {
        for skin in ["dark", "light", "mur", "clay"] {
            let theme = theme::resolve_skin(skin);
            let args = serde_json::json!({
                "file_path": "src/lib.rs", "old_string": "let x = 1;", "new_string": "let x = 2;"
            });
            let lines = edit_diff_lines("edit", &args, theme).unwrap();

            let del = lines
                .iter()
                .find(|l| l.spans.first().is_some_and(|s| s.content.contains('-')))
                .expect("a deletion row");
            let add = lines
                .iter()
                .find(|l| l.spans.first().is_some_and(|s| s.content.contains('+')))
                .expect("an addition row");

            for (row, mark, ink) in [
                (del, theme.diff_del_mark, theme.diff_del_text),
                (add, theme.diff_add_mark, theme.diff_add_text),
            ] {
                assert_eq!(row.spans.len(), 2, "{skin}: row is gutter + code: {row:?}");
                assert_eq!(
                    row.spans[0].style.fg, mark.fg,
                    "{skin}: gutter must carry the skin's sign colour"
                );
                assert_eq!(
                    row.spans[1].style.fg, ink.fg,
                    "{skin}: code text must use the skin's row ink: {:?}",
                    row.spans[1]
                );
            }
        }
    }

    #[test]
    fn edit_args_produce_minus_plus_lines() {        let args = serde_json::json!({
            "file_path": "src/lib.rs", "old_string": "let x = 1;", "new_string": "let x = 2;"
        });
        let lines = edit_diff_lines("edit", &args, theme::resolve_skin("dark")).unwrap();
        let t = text(&lines);
        assert!(
            t.contains("- let x = 1;"),
            "expected removal line, got:\n{t}"
        );
        assert!(
            t.contains("+ let x = 2;"),
            "expected addition line, got:\n{t}"
        );
    }

    #[test]
    fn murs_own_edit_tools_render_a_diff() {
        // `edit_file` / `write_file` are the names MUR's OWN runtime registers
        // (mur-agent-runtime/src/tools/{edit_file,write_file}.rs). The gate
        // listed only the Claude-CLI spellings, so a first-party edit fell
        // through to a raw JSON args dump and never showed a diff.
        let edit = serde_json::json!({
            "path": "src/lib.rs", "old_string": "let x = 1;", "new_string": "let x = 2;"
        });
        let t = text(&edit_diff_lines("edit_file", &edit, theme::resolve_skin("dark")).unwrap());
        assert!(t.contains("- let x = 1;"), "expected removal, got:\n{t}");
        assert!(t.contains("+ let x = 2;"), "expected addition, got:\n{t}");

        let write = serde_json::json!({ "path": "out.txt", "content": "hello" });
        let t = text(&edit_diff_lines("write_file", &write, theme::resolve_skin("dark")).unwrap());
        assert!(t.contains("+ hello"), "expected addition, got:\n{t}");
    }

    #[test]
    fn non_edit_tool_returns_none() {
        let args = serde_json::json!({ "old_string": "a", "new_string": "b" });
        assert!(edit_diff_lines("bash", &args, theme::resolve_skin("dark")).is_none());
        assert!(edit_diff_lines("read", &args, theme::resolve_skin("dark")).is_none());
    }

    #[test]
    fn write_tool_no_old_string_shows_additions() {
        let args = serde_json::json!({
            "file_path": "out.txt",
            "content": "line one\nline two"
        });
        let lines = edit_diff_lines("write", &args, theme::resolve_skin("dark")).unwrap();
        let t = text(&lines);
        assert!(t.contains("+ line one"), "expected + line one, got:\n{t}");
        assert!(t.contains("+ line two"), "expected + line two, got:\n{t}");
    }

    #[test]
    fn long_diffs_remain_complete_for_transcript_scrolling() {
        // The transcript viewport scrolls, so a 50-line edit must retain every
        // row rather than hide the tail behind a local card cap.
        let content = (0..50)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let args = serde_json::json!({ "file_path": "big.rs", "content": content });
        let lines = edit_diff_lines("write", &args, theme::resolve_skin("dark")).unwrap();
        let t = text(&lines);
        assert!(t.contains("+ line 0"), "missing first line:\n{t}");
        assert!(t.contains("+ line 49"), "missing final line:\n{t}");
        assert!(
            !t.contains("more diff line(s)"),
            "expanded diff must not be locally truncated:\n{t}"
        );
    }

    /// Not an assertion — a human eyeball. Renders the same diff under every
    /// skin as real ANSI so the tints can be compared side by side in a
    /// terminal, using the production `Theme` rather than hand-copied hex.
    ///
    ///   cargo test -p mur-core --lib diff::tests::skin_gallery -- --ignored --nocapture
    #[test]
    #[ignore = "visual: prints an ANSI gallery for a human to look at"]
    fn skin_gallery() {
        use ratatui::style::{Color, Modifier};

        fn sgr(style: ratatui::style::Style) -> String {
            let mut out = String::from("\x1b[0m");
            let code = |c: Color, base: u8| -> Option<String> {
                Some(match c {
                    Color::Rgb(r, g, b) => format!("\x1b[{};2;{r};{g};{b}m", base + 8),
                    Color::Red => format!("\x1b[{}m", base + 1),
                    Color::Green => format!("\x1b[{}m", base + 2),
                    Color::Yellow => format!("\x1b[{}m", base + 3),
                    Color::Blue => format!("\x1b[{}m", base + 4),
                    Color::Magenta => format!("\x1b[{}m", base + 5),
                    Color::Cyan => format!("\x1b[{}m", base + 6),
                    Color::Gray => format!("\x1b[{}m", base + 7),
                    Color::DarkGray => format!("\x1b[{};5;240m", base + 8),
                    Color::White => format!("\x1b[{};5;255m", base + 8),
                    _ => return None,
                })
            };
            if let Some(fg) = style.fg.and_then(|c| code(c, 30)) {
                out.push_str(&fg);
            }
            if let Some(bg) = style.bg.and_then(|c| code(c, 40)) {
                out.push_str(&bg);
            }
            if style.add_modifier.contains(Modifier::DIM) {
                out.push_str("\x1b[2m");
            }
            if style.add_modifier.contains(Modifier::BOLD) {
                out.push_str("\x1b[1m");
            }
            out
        }

        let args = serde_json::json!({
            "file_path": "mur-core/src/cmd/agent/cli/diff.rs",
            "old_string": "let mark = theme.error;\nrow.paint_all(mark);",
            "new_string": "let mark = theme.diff_del_mark;\nrow.gutter_only(mark);",
        });

        for skin in ["ansi", "light", "mur", "clay"] {
            let theme = theme::resolve_skin(skin);
            println!("\x1b[0m\n\x1b[1m  ── skin: {skin} ──\x1b[0m");
            for line in edit_diff_lines("edit", &args, theme).unwrap() {
                let mut row = String::new();
                for span in &line.spans {
                    row.push_str(&sgr(line.style.patch(span.style)));
                    row.push_str(span.content.as_ref());
                }
                println!("{row}\x1b[0m");
            }
        }
        println!("\x1b[0m");
    }
}
