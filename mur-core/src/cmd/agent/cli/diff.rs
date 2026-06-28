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
fn is_edit_tool(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "edit" | "write" | "multiedit" | "str_replace" | "str_replace_editor"
    )
}

fn str_field<'a>(args: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| args.get(*k).and_then(|v| v.as_str()))
}

/// Append one bounded diff line; no-op once `pushed` reaches `DIFF_MAX_LINES`.
/// The counter always increments so it reflects the true total line count.
fn push(out: &mut Vec<Line<'static>>, pushed: &mut usize, s: String, color: Color, dim: bool) {
    if *pushed < DIFF_MAX_LINES {
        let mut st = Style::default().fg(color);
        if dim {
            st = st.add_modifier(Modifier::DIM);
        }
        out.push(Line::styled(s, st));
    }
    *pushed += 1; // always increment — drives the accurate "+N more" count
}

/// Build bounded `-`/`+` diff lines for an edit-like tool call, or `None` if
/// this isn't an edit we can render.
pub fn edit_diff_lines(
    name: &str,
    args: &serde_json::Value,
    theme: &'static Theme,
) -> Option<Vec<Line<'static>>> {
    if !is_edit_tool(name) {
        return None;
    }
    let path = str_field(args, &["file_path", "path"]).unwrap_or("");
    let old = str_field(args, &["old_string", "old_str"]);
    let new = str_field(args, &["new_string", "new_str", "content"]);
    // Need at least the `new` side to render anything.
    let new = new?;

    let mut out: Vec<Line<'static>> = Vec::new();
    if !path.is_empty() {
        out.push(Line::styled(
            format!(" {path}"),
            Style::default()
                .fg(theme.system)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let mut pushed = 0usize;

    match old {
        Some(old_str) => {
            // Full diff: context lines (both), removed (Left), added (Right).
            for hunk in diff::lines(old_str, new) {
                match hunk {
                    diff::Result::Left(l) => {
                        push(&mut out, &mut pushed, format!("  - {l}"), Color::Red, false);
                    }
                    diff::Result::Right(r) => {
                        push(
                            &mut out,
                            &mut pushed,
                            format!("  + {r}"),
                            Color::Green,
                            false,
                        );
                    }
                    diff::Result::Both(l, _) => {
                        push(
                            &mut out,
                            &mut pushed,
                            format!("    {l}"),
                            theme.system,
                            true,
                        );
                    }
                }
            }
        }
        None => {
            // No old string: just show the new content as additions.
            for line in new.lines() {
                push(
                    &mut out,
                    &mut pushed,
                    format!("  + {line}"),
                    Color::Green,
                    false,
                );
            }
        }
    }

    // If there were more lines than the cap, append a truncation hint.
    if pushed > DIFF_MAX_LINES {
        out.push(Line::styled(
            format!("  … +{} more diff line(s)", pushed - DIFF_MAX_LINES),
            Style::default()
                .fg(theme.system)
                .add_modifier(Modifier::DIM),
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
