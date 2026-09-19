//! Render an edit-tool's `{file_path, old_string, new_string}` args as a
//! bounded `-`/`+`/context diff, using the `diff` crate.
//!
//! Consumed by `render_card::card_lines` to show an inline diff for edit-tool cards.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

use super::theme::Theme;

/// Hard cap on diff lines rendered (keeps cards short even on large edits).
const DIFF_MAX_LINES: usize = 40;

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

/// Append one bounded diff line; no-op once `pushed` reaches `DIFF_MAX_LINES`.
/// The counter always increments so it reflects the true total line count.
fn push(out: &mut Vec<Line<'static>>, pushed: &mut usize, s: String, style: Style, dim: bool) {
    if *pushed < DIFF_MAX_LINES {
        let mut st = style;
        if dim {
            st = st.add_modifier(Modifier::DIM);
        }
        out.push(Line::styled(s, st));
    }
    *pushed += 1; // always increment — drives the accurate "+N more" count
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

/// Build bounded `-`/`+` diff lines for an edit-like tool call, or `None` if
/// this isn't an edit we can render.
pub fn edit_diff_lines(
    name: &str,
    args: &serde_json::Value,
    theme: &'static Theme,
) -> Option<Vec<Line<'static>>> {
    let parts = edit_diff_parts(name, args)?;

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut pushed = 0usize;

    for part in parts {
        match part {
            // The path header sits above the diff and is never capped.
            DiffPart::Path(p) => out.push(Line::styled(
                format!(" {p}"),
                theme.muted.add_modifier(Modifier::BOLD),
            )),
            DiffPart::Del(l) => push(
                &mut out,
                &mut pushed,
                format!("  - {l}"),
                Style::default().fg(Color::Red),
                false,
            ),
            DiffPart::Add(l) => push(
                &mut out,
                &mut pushed,
                format!("  + {l}"),
                Style::default().fg(Color::Green),
                false,
            ),
            DiffPart::Ctx(l) => push(&mut out, &mut pushed, format!("    {l}"), theme.muted, true),
        }
    }

    // If there were more lines than the cap, append a truncation hint.
    if pushed > DIFF_MAX_LINES {
        out.push(Line::styled(
            format!("  … +{} more diff line(s)", pushed - DIFF_MAX_LINES),
            theme.muted.add_modifier(Modifier::DIM),
        ));
    }

    Some(out)
}

#[cfg(test)]
mod tests {
    use super::edit_diff_lines;
    use crate::cmd::agent::cli::theme;

    fn text(lines: &[ratatui::text::Line]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn edit_args_produce_minus_plus_lines() {
        let args = serde_json::json!({
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
    fn truncation_marker_only_past_cap_with_correct_count() {
        // 50 added lines (write tool, no old_string) → 40 shown + "+10 more"
        let content = (0..50)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let args = serde_json::json!({ "file_path": "big.rs", "content": content });
        let lines = edit_diff_lines("write", &args, theme::resolve_skin("dark")).unwrap();
        let t: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(t.contains("+10 more"), "expected '+10 more' in:\n{t}");

        // exactly 40 lines must NOT fire the marker
        let content40 = (0..40)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let args40 = serde_json::json!({ "file_path": "exact.rs", "content": content40 });
        let lines40 = edit_diff_lines("write", &args40, theme::resolve_skin("dark")).unwrap();
        let t40: String = lines40
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !t40.contains("more"),
            "must NOT show marker at exactly 40 lines, got:\n{t40}"
        );
    }
}
