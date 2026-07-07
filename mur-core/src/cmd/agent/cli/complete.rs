//! Pure autocomplete logic for the `mur agent cli` completion menu: build the
//! candidate set for the current input and filter it. No TUI, no I/O here
//! (except `load_agent_skills`, which reads the agent profile at startup).

use std::collections::HashSet;
use std::path::Path;

/// Most rows shown before the menu scrolls (kept in sync with `ui.rs`).
pub const MAX_MENU_ROWS: usize = 8;

/// One selectable menu entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// What the menu shows in the left column (`/skill`, `list`, `create-pr`).
    pub display: String,
    /// Text that replaces the whole input line on accept (`/mcp `, `/mcp list `,
    /// `create-pr`).
    pub insert: String,
    /// Right-column description (may be empty).
    pub desc: String,
    /// True for a top-level command that has a subcommand layer — accepting it
    /// keeps the menu open and shows layer 2.
    pub has_children: bool,
}

/// The live menu: the filtered candidates plus the highlighted row.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionState {
    pub items: Vec<Candidate>,
    pub selected: usize,
    /// True when this menu is the agent's suggested-reply chooser rather than
    /// the slash-command menu. The chooser renders each option with a blank
    /// spacer row so the choices don't crowd each other.
    pub spaced: bool,
}

/// Built-in commands: (word without slash, description, subcommands).
/// `exit` is omitted as a duplicate of `quit`.
const COMMANDS: &[(&str, &str, &[&str])] = &[
    ("auto", "session-wide auto-approval", &["on", "off"]),
    ("card", "show this agent's card", &[]),
    ("channels", "list or switch channels", &[]),
    ("clear", "start a new conversation", &[]),
    ("help", "show the command cheatsheet", &[]),
    (
        "mcp",
        "manage MCP servers",
        &[
            "list",
            "add",
            "remove",
            "add-remote",
            "login",
            "registry-add",
        ],
    ),
    (
        "panel",
        "companion window (MUR Hub)",
        &[
            "information",
            "activities",
            "preview",
            "notifications",
            "schedule",
            "stream",
        ],
    ),
    ("quit", "exit the chat", &[]),
    ("sessions", "list past sessions", &[]),
    ("skill", "manage agent skills", &["list", "add", "remove"]),
    ("skin", "switch theme", &["dark", "light", "mur"]),
    ("verbose", "expand tool cards", &["on", "off"]),
];

/// Subcommands for `cmd` (without leading slash), or `None` if `cmd` is unknown
/// or takes only free-text args.
fn subcommands_for(cmd: &str) -> Option<&'static [&'static str]> {
    COMMANDS
        .iter()
        .find(|(w, _, _)| *w == cmd)
        .map(|(_, _, subs)| *subs)
        .filter(|subs| !subs.is_empty())
}

/// Top-level candidates: every built-in command plus the agent's skills.
fn build_top_level(skills: &[Candidate]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = COMMANDS
        .iter()
        .map(|(word, desc, subs)| Candidate {
            display: format!("/{word}"),
            insert: format!("/{word} "),
            desc: (*desc).to_string(),
            has_children: !subs.is_empty(),
        })
        .collect();
    out.extend_from_slice(skills);
    out
}

/// Layer-2 candidates for a command word.
fn build_subcommands(cmd: &str, subs: &[&str]) -> Vec<Candidate> {
    subs.iter()
        .map(|sub| Candidate {
            display: (*sub).to_string(),
            insert: format!("/{cmd} {sub} "),
            desc: String::new(),
            has_children: false,
        })
        .collect()
}

/// Case-insensitive substring filter on the candidate word (display minus any
/// leading `/`).
fn filter(cands: Vec<Candidate>, query: &str) -> Vec<Candidate> {
    let q = query.to_lowercase();
    cands
        .into_iter()
        .filter(|c| {
            c.display
                .trim_start_matches('/')
                .to_lowercase()
                .contains(&q)
        })
        .collect()
}

/// Derive the completion menu from the current input. Returns `None` when the
/// input is not in a slash context or nothing matches (menu closed).
pub fn compute(input: &str, skills: &[Candidate]) -> Option<CompletionState> {
    // ponytail: slash commands are single-line; a multiline composer has no menu.
    if input.contains('\n') {
        return None;
    }
    let after = input.trim_start().strip_prefix('/')?;
    let items = match after.split_once(char::is_whitespace) {
        // Still typing the command word.
        None => filter(build_top_level(skills), after),
        // Command word complete → maybe a subcommand layer.
        Some((cmd, rest)) => {
            // A second whitespace means we're typing an arg past layer 2.
            if rest.trim_start().contains(char::is_whitespace) {
                return None;
            }
            let subs = subcommands_for(cmd)?;
            filter(build_subcommands(cmd, subs), rest.trim_start())
        }
    };
    if items.is_empty() {
        return None;
    }
    Some(CompletionState {
        items,
        selected: 0,
        spaced: false,
    })
}

/// Best-effort display name for a skill source string: a path like
/// `.../skills/<name>/skill.yaml` → `<name>`; `<name>.yaml` → `<name>`;
/// a bare name → itself.
pub fn skill_display_name(raw: &str) -> String {
    let p = Path::new(raw);
    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
        if stem == "skill"
            && let Some(parent) = p
                .parent()
                .and_then(|d| d.file_name())
                .and_then(|s| s.to_str())
        {
            return parent.to_string();
        }
        return stem.to_string();
    }
    raw.to_string()
}

/// Load this agent's skills as menu candidates. Fail-soft: any read error
/// yields an empty list (the menu just shows built-in commands). Disabled
/// skills are excluded since they are not injected. ponytail: cached once at
/// startup; mid-session `/skill add` won't refresh it.
pub fn load_agent_skills(agent: &str) -> Vec<Candidate> {
    let Ok((_path, profile)) = crate::cmd::agent::load_profile_for_edit(agent) else {
        return Vec::new();
    };
    let disabled: HashSet<&str> = profile.disabled_skills.iter().map(String::as_str).collect();
    let mut out: Vec<Candidate> = Vec::new();
    for s in &profile.installed_skills {
        if disabled.contains(s.name.as_str()) {
            continue;
        }
        out.push(Candidate {
            display: format!("/{}", s.name),
            insert: format!("/{} ", s.name),
            desc: s.description.clone(),
            has_children: false,
        });
    }
    for raw in &profile.skills {
        let name = skill_display_name(raw);
        let display = format!("/{name}");
        if disabled.contains(name.as_str()) || out.iter().any(|c| c.display == display) {
            continue;
        }
        out.push(Candidate {
            display,
            insert: format!("/{name} "),
            desc: String::new(),
            has_children: false,
        });
    }
    out
}

/// If `line` is a leading-slash invocation whose command word matches one of
/// the agent's `skills` (surfaced in the menu as `/name`), return
/// `(skill_name, trailing_args)`. Callers route this to the agent as a skill
/// invocation instead of the "unknown command" branch. Returns `None` for
/// ordinary input or built-in commands.
pub fn matched_skill(line: &str, skills: &[Candidate]) -> Option<(String, String)> {
    let rest = line.trim().strip_prefix('/')?;
    let (word, args) = match rest.split_once(char::is_whitespace) {
        Some((w, a)) => (w, a.trim()),
        None => (rest, ""),
    };
    let want = format!("/{word}");
    skills
        .iter()
        .find(|c| c.display == want)
        .map(|_| (word.to_string(), args.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str) -> Candidate {
        Candidate {
            display: name.into(),
            insert: name.into(),
            desc: String::new(),
            has_children: false,
        }
    }

    fn displays(state: &CompletionState) -> Vec<String> {
        state.items.iter().map(|c| c.display.clone()).collect()
    }

    #[test]
    fn no_menu_without_leading_slash() {
        assert!(compute("hello", &[skill("create-pr")]).is_none());
    }

    #[test]
    fn matched_skill_resolves_slash_form_and_args() {
        // Real skill candidates carry a leading slash (see load_agent_skills).
        let skills = [skill("/brainstorming"), skill("/create-pr")];
        assert_eq!(
            matched_skill("/brainstorming", &skills),
            Some(("brainstorming".into(), String::new()))
        );
        assert_eq!(
            matched_skill("/create-pr fix the bug", &skills),
            Some(("create-pr".into(), "fix the bug".into()))
        );
        // Non-skill slash words and plain text don't match.
        assert_eq!(matched_skill("/help", &skills), None);
        assert_eq!(matched_skill("hello", &skills), None);
    }

    #[test]
    fn top_level_filters_commands_by_prefix_substring() {
        let s = compute("/sk", &[skill("create-pr")]).unwrap();
        let d = displays(&s);
        assert!(d.contains(&"/skill".to_string()));
        assert!(d.contains(&"/skin".to_string()));
        // "sk" does not match the skill "create-pr".
        assert!(!d.contains(&"create-pr".to_string()));
    }

    #[test]
    fn top_level_includes_matching_skills() {
        let s = compute("/cre", &[skill("create-pr")]).unwrap();
        assert_eq!(displays(&s), vec!["create-pr".to_string()]);
    }

    #[test]
    fn empty_slash_shows_commands_and_skills() {
        let s = compute("/", &[skill("create-pr")]).unwrap();
        let d = displays(&s);
        assert!(d.contains(&"/mcp".to_string()));
        assert!(d.contains(&"create-pr".to_string()));
    }

    #[test]
    fn panel_subcommands() {
        let s = compute("/panel ", &[]).unwrap();
        assert!(s.items.iter().any(|c| c.insert == "/panel preview "));
        assert_eq!(s.items.len(), 6);
    }

    #[test]
    fn descends_to_subcommands_after_space() {
        let s = compute("/mcp ", &[]).unwrap();
        let d = displays(&s);
        assert!(d.contains(&"list".to_string()));
        assert!(d.contains(&"add-remote".to_string()));
        let add = s.items.iter().find(|c| c.display == "list").unwrap();
        assert_eq!(add.insert, "/mcp list ");
        assert!(!add.has_children);
    }

    #[test]
    fn subcommands_filter_by_query() {
        let s = compute("/mcp add", &[]).unwrap();
        let d = displays(&s);
        assert!(d.contains(&"add".to_string()));
        assert!(d.contains(&"add-remote".to_string()));
        assert!(!d.contains(&"list".to_string()));
    }

    #[test]
    fn no_menu_past_layer_two() {
        assert!(compute("/mcp add foo", &[]).is_none());
    }

    #[test]
    fn command_without_subcommands_has_no_layer_two() {
        assert!(compute("/help ", &[]).is_none());
    }

    #[test]
    fn unknown_command_no_match_closes_menu() {
        assert!(compute("/zzz", &[]).is_none());
    }

    #[test]
    fn top_level_command_marks_children() {
        let s = compute("/mc", &[]).unwrap();
        let mcp = s.items.iter().find(|c| c.display == "/mcp").unwrap();
        assert!(mcp.has_children);
        assert_eq!(mcp.insert, "/mcp ");
        let help = compute("/hel", &[]).unwrap();
        let h = help.items.iter().find(|c| c.display == "/help").unwrap();
        assert!(!h.has_children);
    }

    #[test]
    fn skill_display_name_handles_paths_and_names() {
        assert_eq!(skill_display_name("/a/b/skills/foo/skill.yaml"), "foo");
        assert_eq!(skill_display_name("bar.yaml"), "bar");
        assert_eq!(skill_display_name("baz"), "baz");
    }

    #[test]
    fn multiline_input_has_no_menu() {
        assert!(compute("/mcp\nlist", &[]).is_none());
    }
}
