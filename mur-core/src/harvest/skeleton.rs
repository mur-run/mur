//! Command skeletons: strip volatile literals so recurring procedures compare
//! equal across sessions (v2 spec Layer 2 normalization, heuristic subset).

use crate::session::SessionEvent;

/// Replace volatile literals in a shell command with placeholders.
pub fn skeletonize_command(cmd: &str) -> String {
    let mut out = String::with_capacity(cmd.len());
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                // consume to closing quote (or end)
                for q in chars.by_ref() {
                    if q == c {
                        break;
                    }
                }
                out.push_str("<STR>");
            }
            _ => out.push(c),
        }
    }
    // token-level passes
    out.split_whitespace()
        .map(|tok| {
            if tok.starts_with('/') && tok.len() > 1 {
                "<PATH>"
            } else if tok.len() >= 12 && tok.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
                "<ID>"
            } else if !tok.is_empty() && tok.chars().all(|c| c.is_ascii_digit()) {
                "<N>"
            } else {
                tok
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Skeletonize a step list into a matching key, re-deduping consecutive steps
/// that collapse to the same skeleton. `tool:<Name>` markers pass through.
pub fn skeletonize_steps(steps: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in steps {
        let k = if s.starts_with("tool:") {
            s.clone()
        } else {
            skeletonize_command(s)
        };
        if out.last() != Some(&k) {
            out.push(k);
        }
    }
    out
}

/// Extract an ordered, consecutive-deduped list of the commands a session actually
/// ran. Non-shell tools become `tool:<Name>` markers. This is the reviewable
/// artifact; `skeletonize_steps` derives the matching key from it.
pub fn commands_from_events(events: &[SessionEvent]) -> Vec<String> {
    let mut steps: Vec<String> = Vec::new();
    for e in events {
        if e.event_type != "tool_call" {
            continue;
        }
        let step = match e.tool.as_deref() {
            Some("Bash") | Some("shell") => serde_json::from_str::<serde_json::Value>(&e.content)
                .ok()
                .and_then(|v| v.get("command").and_then(|c| c.as_str()).map(str::to_owned))
                .unwrap_or_else(|| e.content.clone()),
            Some(other) => format!("tool:{}", other),
            None => continue,
        };
        if steps.last().map(|s| s.as_str()) != Some(step.as_str()) {
            steps.push(step);
        }
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionEvent;

    fn tool_event(tool: &str, content: &str) -> SessionEvent {
        SessionEvent {
            timestamp: 0,
            event_type: "tool_call".to_string(),
            tool: Some(tool.to_string()),
            content: content.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn strips_quotes_paths_ids_numbers() {
        assert_eq!(
            skeletonize_command(r#"fly deploy --app "my-api" --wait 300"#),
            "fly deploy --app <STR> --wait <N>"
        );
        assert_eq!(skeletonize_command("cat /Users/d/x.txt"), "cat <PATH>");
        assert_eq!(
            skeletonize_command("git checkout 0123abcd4567ef89"),
            "git checkout <ID>"
        );
    }

    #[test]
    fn commands_dedupe_consecutive_and_mark_tools() {
        let events = vec![
            tool_event("Bash", r#"{"command":"cargo build"}"#),
            tool_event("Bash", r#"{"command":"cargo build"}"#),
            tool_event("Read", "src/main.rs"),
            tool_event("Bash", r#"{"command":"cargo test"}"#),
        ];
        assert_eq!(
            commands_from_events(&events),
            vec!["cargo build", "tool:Read", "cargo test"]
        );
    }

    #[test]
    fn commands_keep_literals_skeleton_strips_them() {
        let events = vec![
            tool_event("Bash", r#"{"command":"fly deploy --app \"my-api\""}"#),
            tool_event("Bash", r#"{"command":"fly deploy --app \"my-web\""}"#),
        ];
        let cmds = commands_from_events(&events);
        // The reviewable artifact keeps what was actually typed …
        assert_eq!(
            cmds,
            vec!["fly deploy --app \"my-api\"", "fly deploy --app \"my-web\""]
        );
        // … while the matching key collapses both to one step.
        assert_eq!(skeletonize_steps(&cmds), vec!["fly deploy --app <STR>"]);
    }

    #[test]
    fn skeletonize_steps_passes_tool_markers_through() {
        let steps = vec!["tool:Read".to_string(), "cat /a/b".to_string()];
        assert_eq!(skeletonize_steps(&steps), vec!["tool:Read", "cat <PATH>"]);
    }
}
