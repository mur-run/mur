//! Notion block → markdown converter.
//!
//! Notion's API returns pages as a tree of typed blocks. We linearise to
//! markdown for the existing `markdown::chunk_markdown` to chunk by heading.
//! Block types covered (P1.4): paragraph, heading_1/2/3,
//! bulleted_list_item, numbered_list_item, code, quote, callout, to_do,
//! toggle (recurses), table (rendered as markdown table).
//!
//! Database property pages are NOT included (deferred).

use serde_json::Value;

/// Convert a flat list of Notion block JSON values to a single markdown body.
///
/// `blocks` should be the `results` array from a `/v1/blocks/{id}/children`
/// response. Each element is a typed object like `{ "type": "paragraph",
/// "paragraph": { "rich_text": [...] } }`.
pub fn blocks_to_markdown(blocks: &[Value]) -> String {
    let mut out = String::new();
    for b in blocks {
        render_block(b, 0, &mut out);
    }
    out
}

fn render_block(block: &Value, depth: usize, out: &mut String) {
    let kind = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let detail = block.get(kind);
    let indent = "  ".repeat(depth);
    match kind {
        "paragraph" => {
            push_rich_text(detail, out);
            out.push_str("\n\n");
        }
        "heading_1" => {
            out.push_str("# ");
            push_rich_text(detail, out);
            out.push_str("\n\n");
        }
        "heading_2" => {
            out.push_str("## ");
            push_rich_text(detail, out);
            out.push_str("\n\n");
        }
        "heading_3" => {
            out.push_str("### ");
            push_rich_text(detail, out);
            out.push_str("\n\n");
        }
        "bulleted_list_item" => {
            out.push_str(&indent);
            out.push_str("- ");
            push_rich_text(detail, out);
            out.push('\n');
        }
        "numbered_list_item" => {
            out.push_str(&indent);
            out.push_str("1. ");
            push_rich_text(detail, out);
            out.push('\n');
        }
        "to_do" => {
            let checked = detail.and_then(|d| d.get("checked")).and_then(|v| v.as_bool()).unwrap_or(false);
            out.push_str(&indent);
            out.push_str(if checked { "- [x] " } else { "- [ ] " });
            push_rich_text(detail, out);
            out.push('\n');
        }
        "quote" => {
            out.push_str("> ");
            push_rich_text(detail, out);
            out.push_str("\n\n");
        }
        "callout" => {
            // Render as quote with leading icon-emoji placeholder.
            out.push_str("> ");
            push_rich_text(detail, out);
            out.push_str("\n\n");
        }
        "code" => {
            let lang = detail
                .and_then(|d| d.get("language"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            out.push_str("```");
            out.push_str(lang);
            out.push('\n');
            push_rich_text(detail, out);
            out.push_str("\n```\n\n");
        }
        "toggle" => {
            // Surface the summary line; nested children handled if expanded by caller
            push_rich_text(detail, out);
            out.push('\n');
            if let Some(children) = block.get("children").and_then(|v| v.as_array()) {
                for child in children {
                    render_block(child, depth + 1, out);
                }
            }
        }
        "table" => {
            // Children of a table are `table_row` blocks (caller must hydrate children).
            if let Some(rows) = block.get("children").and_then(|v| v.as_array()) {
                render_table(rows, out);
            }
        }
        "divider" => {
            out.push_str("\n---\n\n");
        }
        _ => {
            // Unknown / unhandled: best-effort rich_text extraction for resilience
            if let Some(detail) = detail {
                push_rich_text(Some(detail), out);
                out.push_str("\n\n");
            }
        }
    }
    // Most blocks support nested children; fetch+inject when caller hydrates.
    // For toggles and table we already handled above. For other types, render
    // children if present (e.g., bulleted lists contain sub-items).
    if !matches!(kind, "toggle" | "table") {
        if let Some(children) = block.get("children").and_then(|v| v.as_array()) {
            for child in children {
                render_block(child, depth + 1, out);
            }
        }
    }
}

fn push_rich_text(detail: Option<&Value>, out: &mut String) {
    let arr = detail
        .and_then(|d| d.get("rich_text"))
        .and_then(|v| v.as_array());
    let Some(arr) = arr else { return };
    for span in arr {
        if let Some(t) = span.get("plain_text").and_then(|v| v.as_str()) {
            out.push_str(t);
        }
    }
}

fn render_table(rows: &[Value], out: &mut String) {
    let mut first = true;
    for row in rows {
        if row.get("type").and_then(|v| v.as_str()) != Some("table_row") {
            continue;
        }
        let cells = row
            .get("table_row")
            .and_then(|d| d.get("cells"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        out.push('|');
        for cell in &cells {
            out.push(' ');
            if let Some(cell_arr) = cell.as_array() {
                for span in cell_arr {
                    if let Some(t) = span.get("plain_text").and_then(|v| v.as_str()) {
                        out.push_str(t);
                    }
                }
            }
            out.push_str(" |");
        }
        out.push('\n');
        if first {
            out.push('|');
            for _ in &cells {
                out.push_str(" --- |");
            }
            out.push('\n');
            first = false;
        }
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn paragraph_to_markdown() {
        let blocks = vec![json!({
            "type": "paragraph",
            "paragraph": {"rich_text": [{"plain_text": "hello world"}]}
        })];
        let md = blocks_to_markdown(&blocks);
        assert!(md.starts_with("hello world"));
    }

    #[test]
    fn heading_1_renders_hash() {
        let blocks = vec![json!({
            "type": "heading_1",
            "heading_1": {"rich_text": [{"plain_text": "Title"}]}
        })];
        assert!(blocks_to_markdown(&blocks).contains("# Title"));
    }

    #[test]
    fn bulleted_list_item_renders_dash() {
        let blocks = vec![json!({
            "type": "bulleted_list_item",
            "bulleted_list_item": {"rich_text": [{"plain_text": "item"}]}
        })];
        assert!(blocks_to_markdown(&blocks).contains("- item"));
    }

    #[test]
    fn to_do_unchecked_renders_brackets() {
        let blocks = vec![json!({
            "type": "to_do",
            "to_do": {"rich_text": [{"plain_text": "task"}], "checked": false}
        })];
        assert!(blocks_to_markdown(&blocks).contains("- [ ] task"));
    }

    #[test]
    fn code_block_includes_language_fence() {
        let blocks = vec![json!({
            "type": "code",
            "code": {"language": "rust", "rich_text": [{"plain_text": "let x = 1;"}]}
        })];
        let md = blocks_to_markdown(&blocks);
        assert!(md.contains("```rust"));
        assert!(md.contains("let x = 1;"));
    }

    #[test]
    fn unknown_block_extracts_rich_text() {
        let blocks = vec![json!({
            "type": "embed",
            "embed": {"rich_text": [{"plain_text": "https://x"}]}
        })];
        assert!(blocks_to_markdown(&blocks).contains("https://x"));
    }
}
