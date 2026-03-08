//! Extract workflow drafts from session recordings.
//!
//! Pure business logic — no HTTP types. Called by the server handler.

use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::Content;
use mur_common::workflow::{Step, VarType, Variable, Workflow};

/// Result of extracting a workflow from session events.
pub struct ExtractedWorkflow {
    pub workflow: Workflow,
}

/// Extract a draft workflow from session events.
pub fn extract_workflow(
    session_id: &str,
    events: &[crate::session::SessionEvent],
) -> ExtractedWorkflow {
    // ── Noise filter ────────────────────────────────────────────────
    let noise_patterns = [
        "mur session start",
        "mur session stop",
        "mur session record",
        "mur sync",
        "mur context",
        "mur inject",
        "/mur:in",
        "/mur:out",
        "/mur-in",
        "/mur-out",
        "[stop: turn_end]",
        "[stop:",
        "turn_end",
    ];

    let is_noise = |evt: &crate::session::SessionEvent| -> bool {
        let c = evt.content.to_lowercase();
        noise_patterns.iter().any(|n| c.contains(n)) || evt.content.trim().is_empty()
    };

    // ── Extract user intent ─────────────────────────────────────────
    let first_user_msg = events
        .iter()
        .find(|e| e.event_type == "user" && !is_noise(e))
        .map(|e| e.content.trim().to_string());

    // ── Filter tool_calls ───────────────────────────────────────────
    let tool_calls: Vec<&crate::session::SessionEvent> = events
        .iter()
        .filter(|e| e.event_type == "tool_call" && !is_noise(e))
        .collect();

    // ── Detect tools/agents ─────────────────────────────────────────
    let mut detected_tools = std::collections::HashSet::new();

    for evt in &tool_calls {
        if let Some(ref tool) = evt.tool {
            detected_tools.insert(tool.clone());
        }
        let c = &evt.content;
        for prefix in ["agent-browser", "agent-", "mcp-server-", "mcp_server_"] {
            if let Some(pos) = c.find(prefix) {
                let name: String = c[pos..]
                    .chars()
                    .take_while(|ch| ch.is_alphanumeric() || *ch == '-' || *ch == '_')
                    .collect();
                if !name.is_empty() {
                    detected_tools.insert(name);
                }
            }
        }
    }

    let mut tools: Vec<String> = detected_tools.into_iter().collect();
    tools.sort();

    // ── Build steps ─────────────────────────────────────────────────
    let steps: Vec<Step> = tool_calls
        .iter()
        .enumerate()
        .map(|(i, evt)| {
            let tool_name = evt.tool.clone().unwrap_or_default();
            let (parsed_cmd, parsed_desc) = parse_tool_content(&evt.content);

            let description = parsed_desc.unwrap_or_else(|| {
                if let Some(ref cmd) = parsed_cmd {
                    let short = if cmd.len() > 80 {
                        format!("{}…", &cmd[..80])
                    } else {
                        cmd.clone()
                    };
                    format!("{}: {}", tool_name, short)
                } else if evt.content.len() > 120 {
                    format!("{}: {}…", tool_name, &evt.content[..120])
                } else if tool_name.is_empty() {
                    evt.content.clone()
                } else {
                    format!("{}: {}", tool_name, evt.content)
                }
            });

            let command = parsed_cmd.or_else(|| {
                if tool_name == "Bash" {
                    Some(evt.content.clone())
                } else {
                    None
                }
            });

            Step {
                order: (i + 1) as u32,
                description,
                command,
                tool: evt.tool.clone(),
                needs_approval: false,
                on_failure: Default::default(),
            }
        })
        .collect();

    // ── Detect variables ────────────────────────────────────────────
    let variables = detect_variables(first_user_msg.as_deref(), &tools);

    // ── Generate title ──────────────────────────────────────────────
    let name = generate_title(first_user_msg.as_deref(), &variables, &tools, session_id);

    // ── Generate description ────────────────────────────────────────
    let description = generate_description(first_user_msg.as_deref(), &tools);

    let workflow = Workflow {
        base: KnowledgeBase {
            name,
            description,
            content: Content::Plain(first_user_msg.unwrap_or_default()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ..Default::default()
        },
        steps,
        tools,
        source_sessions: vec![session_id.to_string()],
        trigger: String::new(),
        variables,
        published_version: 0,
        permission: Default::default(),
    };

    ExtractedWorkflow { workflow }
}

/// Parse command/description from JSON tool content.
fn parse_tool_content(content: &str) -> (Option<String>, Option<String>) {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(content) {
        let cmd = v.get("command").and_then(|c| c.as_str()).map(String::from);
        let desc = v
            .get("description")
            .and_then(|d| d.as_str())
            .map(String::from);
        return (cmd, desc);
    }
    (None, None)
}

/// Detect variables from the user's message.
fn detect_variables(msg: Option<&str>, _tools: &[String]) -> Vec<Variable> {
    let mut variables = Vec::new();
    let Some(msg) = msg else {
        return variables;
    };

    // 1. Quoted strings (straight + curly quotes)
    let mut in_quote = false;
    let mut close_char = '\'';
    let mut current = String::new();
    let mut quoted_values = Vec::new();

    let open_close: &[(char, char)] = &[
        ('\'', '\''),
        ('"', '"'),
        ('\u{2018}', '\u{2019}'),
        ('\u{201C}', '\u{201D}'),
    ];

    for ch in msg.chars() {
        if !in_quote {
            if let Some(&(_, close)) = open_close.iter().find(|&&(open, _)| open == ch) {
                in_quote = true;
                close_char = close;
                current.clear();
            }
        } else if ch == close_char {
            in_quote = false;
            if !current.trim().is_empty() {
                quoted_values.push(current.trim().to_string());
            }
        } else {
            current.push(ch);
        }
    }

    if let Some(subject) = quoted_values.first() {
        variables.push(Variable {
            name: "product_name".to_string(),
            var_type: VarType::String,
            required: true,
            default_value: Some(subject.clone()),
            description: "Target product or search term".to_string(),
        });
    }

    let words: Vec<&str> = msg.split_whitespace().collect();

    // 2. URLs
    for word in &words {
        let w =
            word.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '/' && c != ':');
        if w.starts_with("http://") || w.starts_with("https://") {
            variables.push(Variable {
                name: "url".to_string(),
                var_type: VarType::Url,
                required: true,
                default_value: Some(w.to_string()),
                description: "Target URL".to_string(),
            });
        }
    }

    // 3. Site names after "in" / "on" / "from" (e.g., "in pchome", "on Amazon")
    let site_prepositions = ["in", "on", "from", "at"];
    let noise_words = [
        "the", "this", "that", "and", "for", "it", "them", "all", "each", "order", "browser",
        "terminal", "parallel", "sequence", "markdown", "json",
    ];
    for (i, word) in words.iter().enumerate() {
        if i == 0 {
            continue;
        }
        let prev = words[i - 1].to_lowercase();
        if !site_prepositions.contains(&prev.as_str()) {
            continue;
        }
        let w = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '-');
        if w.len() > 2
            && w.chars()
                .all(|c| c.is_alphanumeric() || c == '.' || c == '-')
            && !noise_words.contains(&w.to_lowercase().as_str())
            && !variables
                .iter()
                .any(|v| v.name == "url" || v.name == "target_site")
        {
            variables.push(Variable {
                name: "target_site".to_string(),
                var_type: VarType::String,
                required: true,
                default_value: Some(w.to_string()),
                description: "Website or service to search in".to_string(),
            });
        }
    }

    // 4. File paths
    for word in &words {
        let w = word.trim_matches(|c: char| c == '\'' || c == '"');
        if (w.starts_with('/') || w.starts_with("~/") || w.starts_with("./"))
            && w.len() > 2
            && !variables.iter().any(|v| v.name == "file_path")
        {
            variables.push(Variable {
                name: "file_path".to_string(),
                var_type: VarType::Path,
                required: true,
                default_value: Some(w.to_string()),
                description: "File or directory path".to_string(),
            });
        }
    }

    // 5. Numbers with units (e.g., "top 5", "last 10", "3 pages")
    let quantity_triggers = ["top", "last", "first", "limit", "max", "min"];
    for (i, word) in words.iter().enumerate() {
        let w = word.trim_matches(|c: char| !c.is_alphanumeric());
        if let Ok(_n) = w.parse::<u32>() {
            // Check if preceded by a quantity trigger
            let has_trigger =
                i > 0 && quantity_triggers.contains(&words[i - 1].to_lowercase().as_str());
            if has_trigger && !variables.iter().any(|v| v.name == "count") {
                variables.push(Variable {
                    name: "count".to_string(),
                    var_type: VarType::Number,
                    required: false,
                    default_value: Some(w.to_string()),
                    description: "Number of items to process".to_string(),
                });
            }
        }
    }

    // 6. Capitalized multi-word names (likely product/proper nouns) — only if no quoted values found
    if quoted_values.is_empty() {
        let action_words = [
            "find", "search", "get", "check", "compare", "buy", "look", "fetch", "download",
            "install", "update", "review", "analyze", "monitor",
        ];
        let stop_words = [
            "the", "a", "an", "in", "on", "at", "for", "to", "of", "and", "or", "with", "from",
            "by", "is", "it", "be", "use", "using", "prices", "price", "cost", "results", "data",
            "info", "details",
        ];

        // Find consecutive capitalized words (2+ words) after an action verb
        let mut found_action = false;
        let mut cap_run: Vec<&str> = Vec::new();

        for word in &words {
            let clean = word.trim_matches(|c: char| !c.is_alphanumeric());
            if action_words.contains(&clean.to_lowercase().as_str()) {
                found_action = true;
                cap_run.clear();
                continue;
            }
            if found_action && !clean.is_empty() {
                let first_char = clean.chars().next().unwrap_or(' ');
                if first_char.is_uppercase() && !stop_words.contains(&clean.to_lowercase().as_str())
                {
                    cap_run.push(clean);
                } else if !cap_run.is_empty() {
                    // End of capitalized run
                    break;
                }
            }
        }

        if cap_run.len() >= 2 {
            let name = cap_run.join(" ");
            if !variables.iter().any(|v| v.name == "product_name") {
                variables.push(Variable {
                    name: "product_name".to_string(),
                    var_type: VarType::String,
                    required: true,
                    default_value: Some(name),
                    description: "Target product or search term".to_string(),
                });
            }
        }
    }

    variables
}

/// Generate concise workflow title from user message.
fn generate_title(
    msg: Option<&str>,
    variables: &[Variable],
    tools: &[String],
    session_id: &str,
) -> String {
    let stop_words = [
        "a", "an", "the", "to", "of", "in", "on", "at", "for", "and", "or", "is", "it", "be",
        "use", "using", "with", "from", "by", "that", "this", "should", "can", "you", "if", "have",
        "has", "then", "first", "also", "notice", "note",
    ];

    let Some(msg) = msg else {
        return format!("session-{}", &session_id[..8.min(session_id.len())]);
    };

    // Remove quoted strings (they're variables)
    let mut clean = msg.to_string();
    for qv in variables {
        if let Some(ref dv) = qv.default_value {
            clean = clean.replace(&format!("'{}'", dv), "");
            clean = clean.replace(&format!("\"{}\"", dv), "");
            clean = clean.replace(dv, "");
        }
    }

    let meaningful: Vec<String> = clean
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .filter(|w| !stop_words.contains(&w.as_str()))
        .filter(|w| w.len() > 1)
        .filter(|w| !tools.iter().any(|t| t.to_lowercase().contains(w)))
        .take(4)
        .collect();

    if meaningful.is_empty() {
        format!("session-{}", &session_id[..8.min(session_id.len())])
    } else {
        meaningful.join("-")
    }
}

/// Generate workflow description from user message + tools.
fn generate_description(msg: Option<&str>, tools: &[String]) -> String {
    let tools_str = if tools.is_empty() {
        String::new()
    } else {
        format!(" Uses {}.", tools.join(", "))
    };

    match msg {
        Some(msg) if msg.len() <= 100 => {
            format!("{}.{}", msg.trim_end_matches('.'), tools_str)
        }
        Some(msg) => {
            let short: String = msg.chars().take(100).collect();
            format!("{}…{}", short.trim_end_matches('.'), tools_str)
        }
        None => {
            let ts = if tools.is_empty() {
                "various tools".to_string()
            } else {
                tools.join(", ")
            };
            format!("Extracted workflow using {}.", ts)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_variables_quoted() {
        let vars = detect_variables(
            Some("find 'AirPods Pro 3' in pchome"),
            &["agent-browser".to_string()],
        );
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].name, "product_name");
        assert_eq!(vars[0].default_value, Some("AirPods Pro 3".to_string()));
        assert_eq!(vars[1].name, "target_site");
        assert_eq!(vars[1].default_value, Some("pchome".to_string()));
    }

    #[test]
    fn test_detect_variables_smart_quotes() {
        let vars = detect_variables(Some("find \u{2018}AirPods\u{2019} in momo"), &[]);
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].default_value, Some("AirPods".to_string()));
    }

    #[test]
    fn test_detect_variables_url() {
        let vars = detect_variables(Some("check https://example.com for updates"), &[]);
        assert!(
            vars.iter()
                .any(|v| v.name == "url" && v.var_type == VarType::Url)
        );
    }

    #[test]
    fn test_detect_variables_file_path() {
        let vars = detect_variables(Some("read ~/Documents/report.pdf and summarize"), &[]);
        assert!(
            vars.iter()
                .any(|v| v.name == "file_path" && v.var_type == VarType::Path)
        );
    }

    #[test]
    fn test_detect_variables_count() {
        let vars = detect_variables(Some("find top 5 results for shoes"), &[]);
        assert!(
            vars.iter()
                .any(|v| v.name == "count" && v.default_value == Some("5".to_string()))
        );
    }

    #[test]
    fn test_detect_variables_capitalized_product() {
        let vars = detect_variables(Some("find AirPods Pro 3 prices on pchome"), &[]);
        assert!(vars.iter().any(|v| v.name == "product_name"));
        assert!(vars.iter().any(|v| v.name == "target_site"));
    }

    #[test]
    fn test_detect_variables_on_preposition() {
        let vars = detect_variables(Some("search for prices on Amazon"), &[]);
        assert!(
            vars.iter()
                .any(|v| v.name == "target_site" && v.default_value == Some("Amazon".to_string()))
        );
    }

    #[test]
    fn test_generate_title() {
        let vars = vec![Variable {
            name: "product_name".to_string(),
            var_type: VarType::String,
            required: true,
            default_value: Some("AirPods Pro 3".to_string()),
            description: String::new(),
        }];
        let title = generate_title(
            Some("use agent-browser to find the prices of 'AirPods Pro 3' in pchome"),
            &vars,
            &["agent-browser".to_string()],
            "abcdef12",
        );
        assert!(title.contains("find"));
        assert!(title.contains("prices"));
        assert!(!title.contains("use"));
        assert!(!title.contains("the"));
    }

    #[test]
    fn test_generate_title_no_msg() {
        let title = generate_title(None, &[], &[], "abcdef123456");
        assert_eq!(title, "session-abcdef12");
    }

    #[test]
    fn test_generate_description() {
        let desc = generate_description(Some("find prices"), &["agent-browser".to_string()]);
        assert!(desc.contains("find prices"));
        assert!(desc.contains("agent-browser"));
    }
}
