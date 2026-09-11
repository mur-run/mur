//! Pure autocomplete logic for the `mur agent cli` completion menu: build the
//! candidate set for the current input and filter it. No TUI and no I/O on the
//! per-keystroke path — the two functions that read disk (`load_agent_skills`
//! and `MenuContext::load`) are called at startup and after a slash command,
//! never from `compute`.

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
    /// The row whose word is in force right now — the model the agent runs,
    /// the effort level that applies, the skin on screen — so a settings menu
    /// shows where you are before you move. `None` for action menus.
    pub current: Option<usize>,
}

/// The argument lists a menu row can come from, read from disk.
///
/// `compute` is pure, so everything it needs that lives in a file is gathered
/// here first. Rebuilt after every slash command (see `mod.rs`), which is what
/// keeps `/effort` honest after a `/model` hot-switch: the levels are a
/// property of the model, and the model can change mid-session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MenuContext {
    /// Effort levels this agent's model accepts, in the model's own order.
    /// Empty when the model takes no reasoning parameter at all.
    pub effort: Vec<String>,
    /// Registry aliases, each with the raw model id behind it.
    pub models: Vec<(String, String)>,
    /// Secret KEYs the agent already holds.
    pub secrets: Vec<String>,
    /// Agent-local note names, newest first, led by the literal `last`.
    pub notes: Vec<String>,
    /// The profile's `effort`, the raw model id and the registry alias behind
    /// it: the on-disk half of "what is in force" (`current_word`).
    pub profile_effort: Option<mur_common::llm::Effort>,
    pub model_id: String,
    pub model_ref: Option<String>,
}

/// The session half of "what is in force": a `/effort` override or a `/skin`
/// switch never touches the profile, so `MenuContext::load` cannot see them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Current {
    pub session_effort: Option<mur_common::llm::Effort>,
    pub auto: bool,
    pub verbose: bool,
    pub skin: &'static str,
}

impl Default for Current {
    fn default() -> Self {
        Self {
            session_effort: None,
            auto: false,
            verbose: false,
            skin: "ansi",
        }
    }
}

impl MenuContext {
    /// Read all four lists. Fail-soft throughout: any list that cannot be read
    /// stays empty and its command simply opens no argument menu.
    pub fn load(home: &Path, agent: &str) -> Self {
        let model_ref = super::model_cmd::current_model_ref(home, agent);
        let model_id = super::model_cmd::current_model_id(home, agent).unwrap_or_default();
        // The levels are NOT a fixed scale — Opus 4.6 has no `xhigh`,
        // DeepSeek V4 has no `medium`, Qwen is a two-position switch. Ask
        // the table keyed on the raw model id; never restate it here.
        let effort = if model_id.is_empty() {
            Vec::new()
        } else {
            mur_common::llm::effort_shape(&model_id)
                .levels()
                .iter()
                .map(|e| e.as_str().to_string())
                .collect()
        };
        let profile_effort = super::model_cmd::current_effort(home, agent);
        let models = mur_common::model::ModelRegistry::default_path()
            .and_then(|p| mur_common::model::ModelRegistry::load_from(&p))
            .map(|reg| {
                super::model_cmd::ordered_models(&reg)
                    .into_iter()
                    .map(|(alias, e)| (alias, e.model))
                    .collect()
            })
            .unwrap_or_default();
        // NOTE the asymmetry: `load_profile_for_edit` resolves the MUR home
        // itself and ignores `home`, exactly as `load_agent_skills` does. Do
        // not "fix" it by threading `home` through — that is a wider change
        // than this menu, and both callers here are the same process reading
        // its own agent.
        let secrets = crate::cmd::agent::load_profile_for_edit(agent)
            .map(|(_path, p)| p.secrets)
            .unwrap_or_default();
        let mut notes = super::memory_cmds::live_note_names(home, agent);
        if !notes.is_empty() {
            // `last` is what `/forget` resolves to, so it belongs in the menu
            // beside the names — and first, because it is the common case.
            notes.insert(0, "last".to_string());
        }
        Self {
            effort,
            models,
            secrets,
            notes,
            profile_effort,
            model_id,
            model_ref,
        }
    }
}

/// Where a command's second-layer rows come from.
///
/// The static-list version of this field is what made `/effort` unrepresentable:
/// its levels are a property of the agent's model, so there was no literal list
/// to write and the command was left out of the menu entirely.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Args {
    /// No second layer.
    None,
    /// Literal words, each with a description for the right-hand column.
    Fixed(&'static [(&'static str, &'static str)]),
    /// `MenuContext::effort` — the levels THIS agent's model accepts.
    Effort,
    /// `MenuContext::models` — registry aliases.
    Model,
    /// `MenuContext::secrets` — KEYs already held, plus `--delete`.
    Secret,
    /// `MenuContext::notes` — agent-local note names, plus `last`.
    Note,
}

const ON_OFF: &[(&str, &str)] = &[("on", "enable"), ("off", "disable")];
const SKINS: &[(&str, &str)] = &[
    ("ansi", "default — follows your terminal"),
    ("light", "light terminals"),
    ("mur", "MUR brand"),
];
const MCP_SUBS: &[(&str, &str)] = &[
    ("list", "servers this agent has"),
    ("add", "add a stdio server"),
    ("remove", "remove a server"),
    ("add-remote", "add a Streamable HTTP server"),
    ("login", "authenticate a remote server"),
    ("registry-add", "add from the MCP registry"),
];
const SKILL_SUBS: &[(&str, &str)] = &[
    ("list", "skills this agent has"),
    ("add", "install a skill"),
    ("remove", "uninstall a skill"),
];
const PANEL_TABS: &[(&str, &str)] = &[
    ("information", ""),
    ("activities", ""),
    ("preview", ""),
    ("notifications", ""),
    ("schedule", ""),
    ("stream", ""),
];
const LOGIN_PROVIDERS: &[(&str, &str)] = &[
    ("anthropic", "Claude subscription"),
    ("chatgpt", "ChatGPT subscription"),
];
const CHANNELS_ARGS: &[(&str, &str)] = &[("--follow", "live-tail another channel")];

/// Built-in commands: (word without slash, description, argument source).
/// `exit` is omitted as a duplicate of `quit`.
///
/// Every command the parser accepts must appear here — the guard test
/// `every_command_is_parsed_documented_and_offered` in `mod.rs` enforces it.
const COMMANDS: &[(&str, &str, Args)] = &[
    ("auto", "session-wide auto-approval", Args::Fixed(ON_OFF)),
    ("card", "show this agent's card", Args::None),
    (
        "channels",
        "list, switch, or follow channels",
        Args::Fixed(CHANNELS_ARGS),
    ),
    ("clear", "start a new conversation", Args::None),
    ("effort", "reasoning effort for this model", Args::Effort),
    ("forget", "drop an agent-local memory", Args::Note),
    ("help", "show the command cheatsheet", Args::None),
    (
        "login",
        "OAuth health / re-authenticate",
        Args::Fixed(LOGIN_PROVIDERS),
    ),
    ("mcp", "manage MCP servers", Args::Fixed(MCP_SUBS)),
    ("memories", "list this agent's memories", Args::None),
    ("model", "list or hot-switch the model", Args::Model),
    ("open", "what is still outstanding", Args::None),
    (
        "panel",
        "companion window (MUR Hub)",
        Args::Fixed(PANEL_TABS),
    ),
    ("quit", "exit the chat", Args::None),
    ("remember", "save an agent-local memory", Args::None),
    (
        "secret",
        "hand the agent a credential (hidden input)",
        Args::Secret,
    ),
    ("sessions", "list past sessions", Args::None),
    ("skill", "manage agent skills", Args::Fixed(SKILL_SUBS)),
    ("skin", "switch theme", Args::Fixed(SKINS)),
    ("verbose", "expand tool cards", Args::Fixed(ON_OFF)),
];

/// Does the completion menu offer this command word? The guard test in
/// `mod.rs` uses it to tie the menu to the parser and to `HELP`.
///
/// Test-only on purpose: nothing in production asks this question, and the
/// `mur` binary target compiles these modules too, so an unconditional `pub fn`
/// here is dead code under `-D warnings`.
#[cfg(test)]
pub fn offers(word: &str) -> bool {
    COMMANDS.iter().any(|(w, _, _)| *w == word)
}

/// The argument source for `cmd` (without leading slash), or `None` if `cmd`
/// is unknown.
fn args_for(cmd: &str) -> Option<Args> {
    COMMANDS
        .iter()
        .find(|(w, _, _)| *w == cmd)
        .map(|(_, _, a)| *a)
}

/// Layer-2 candidates for a command word, resolved against `ctx`.
fn build_args(cmd: &str, args: Args, ctx: &MenuContext) -> Vec<Candidate> {
    let rows: Vec<(String, String)> = match args {
        Args::None => return Vec::new(),
        Args::Fixed(f) => f
            .iter()
            .map(|(w, d)| ((*w).to_string(), (*d).to_string()))
            .collect(),
        // No description column: any wording would be invented. Which level
        // is in force is `CompletionState::current`, computed with the
        // session override in hand, not a label written here from disk.
        Args::Effort => ctx
            .effort
            .iter()
            .map(|l| (l.clone(), String::new()))
            .collect(),
        Args::Model => ctx.models.clone(),
        Args::Note => ctx
            .notes
            .iter()
            .map(|n| (n.clone(), String::new()))
            .collect(),
        Args::Secret => {
            let mut v: Vec<(String, String)> = ctx
                .secrets
                .iter()
                .map(|k| (k.clone(), "already set — replaces it".to_string()))
                .collect();
            v.push(("--delete".to_string(), "revoke a credential".to_string()));
            v
        }
    };
    rows.into_iter()
        .map(|(word, desc)| Candidate {
            display: word.clone(),
            insert: format!("/{cmd} {word} "),
            desc,
            has_children: false,
        })
        .collect()
}

/// Top-level candidates: every built-in command plus the agent's skills.
///
/// `has_children` is derived from whether the command actually has rows to
/// show right now, not merely from its declared source: `/effort` on a model
/// that takes no reasoning parameter has an `Effort` source and no rows, and
/// promising a layer that never opens is worse than promising nothing.
fn build_top_level(skills: &[Candidate], ctx: &MenuContext) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = COMMANDS
        .iter()
        .map(|(word, desc, args)| Candidate {
            display: format!("/{word}"),
            insert: format!("/{word} "),
            desc: (*desc).to_string(),
            has_children: !build_args(word, *args, ctx).is_empty(),
        })
        .collect();
    out.extend_from_slice(skills);
    out
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

/// The word in force for a settings command — the row `compute` marks.
/// Action commands (`/mcp add`, `/login`, …) have no such word.
fn current_word(cmd: &str, ctx: &MenuContext, cur: &Current) -> Option<String> {
    let on_off = |b: bool| Some(if b { "on" } else { "off" }.to_string());
    match cmd {
        "auto" => on_off(cur.auto),
        "verbose" => on_off(cur.verbose),
        "skin" => Some(cur.skin.to_string()),
        "model" => ctx.model_ref.clone(),
        // Unset on both sides marks nothing: the API default is the model's
        // business, and a ✔ on a guess would be a lie.
        "effort" => {
            mur_common::llm::effective_effort(cur.session_effort, ctx.profile_effort, &ctx.model_id)
                .0
                .map(|e| e.as_str().to_string())
        }
        _ => None,
    }
}

/// Derive the completion menu from the current input. Returns `None` when the
/// input is not in a slash context or nothing matches (menu closed).
pub fn compute(
    input: &str,
    skills: &[Candidate],
    ctx: &MenuContext,
    cur: &Current,
) -> Option<CompletionState> {
    // ponytail: slash commands are single-line; a multiline composer has no menu.
    if input.contains('\n') {
        return None;
    }
    let after = input.trim_start().strip_prefix('/')?;
    let (items, current) = match after.split_once(char::is_whitespace) {
        // Still typing the command word.
        None => (filter(build_top_level(skills, ctx), after), None),
        // Command word complete → maybe an argument layer.
        Some((cmd, rest)) => {
            // A second whitespace means we're typing an arg past layer 2.
            if rest.trim_start().contains(char::is_whitespace) {
                return None;
            }
            let args = args_for(cmd)?;
            let items = filter(build_args(cmd, args, ctx), rest.trim_start());
            // Looked up AFTER the filter so the index is into the rows shown.
            let current =
                current_word(cmd, ctx, cur).and_then(|w| items.iter().position(|c| c.display == w));
            (items, current)
        }
    };
    if items.is_empty() {
        return None;
    }
    Some(CompletionState {
        items,
        selected: 0,
        spaced: false,
        current,
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

    fn ctx() -> MenuContext {
        MenuContext {
            effort: vec!["low".into(), "high".into(), "max".into()],
            models: vec![
                ("fast".into(), "deepseek-v4".into()),
                ("smart".into(), "claude-opus-5".into()),
            ],
            secrets: vec!["GITHUB_TOKEN".into()],
            notes: vec!["last".into(), "note-20260908-101500".into()],
            ..MenuContext::default()
        }
    }

    fn cur() -> Current {
        Current::default()
    }

    /// Every settings menu marks the value in force — from the profile for
    /// the model, from the session first for effort, from `App` for the rest —
    /// and every action menu marks nothing.
    #[test]
    fn settings_menus_mark_the_value_in_force() {
        use mur_common::llm::Effort;
        let c = MenuContext {
            model_ref: Some("smart".into()),
            model_id: "claude-opus-5".into(),
            profile_effort: Some(Effort::High),
            ..ctx()
        };
        let cur = Current {
            auto: true,
            verbose: false,
            skin: "mur",
            ..cur()
        };
        let at = |input: &str, cur: &Current| {
            let s = compute(input, &[], &c, cur).unwrap();
            s.current.map(|i| s.items[i].display.clone())
        };
        assert_eq!(at("/model ", &cur).as_deref(), Some("smart"));
        assert_eq!(at("/effort ", &cur).as_deref(), Some("high"), "profile");
        let overridden = Current {
            session_effort: Some(Effort::Low),
            ..cur
        };
        assert_eq!(
            at("/effort ", &overridden).as_deref(),
            Some("low"),
            "session wins"
        );
        assert_eq!(at("/skin ", &cur).as_deref(), Some("mur"));
        assert_eq!(at("/auto ", &cur).as_deref(), Some("on"));
        assert_eq!(at("/verbose ", &cur).as_deref(), Some("off"));
        // The index is into the filtered rows, not the full list.
        let s = compute("/skin m", &[], &c, &cur).unwrap();
        assert_eq!(s.current, Some(0));
        assert_eq!(s.items[0].display, "mur");
        // Action menus and an unset effort mark nothing.
        assert_eq!(compute("/mcp ", &[], &c, &cur).unwrap().current, None);
        let unset = MenuContext {
            profile_effort: None,
            ..c.clone()
        };
        assert_eq!(
            compute("/effort ", &[], &unset, &cur).unwrap().current,
            None
        );
        assert_eq!(compute("/model ", &[], &MenuContext::default(), &cur), None);
    }

    fn effort_ctx(model: &str) -> MenuContext {
        MenuContext {
            effort: mur_common::llm::effort_shape(model)
                .levels()
                .iter()
                .map(|e| e.as_str().to_string())
                .collect(),
            ..MenuContext::default()
        }
    }

    fn displays(state: &CompletionState) -> Vec<String> {
        state.items.iter().map(|c| c.display.clone()).collect()
    }

    #[test]
    fn no_menu_without_leading_slash() {
        assert!(compute("hello", &[skill("create-pr")], &ctx(), &cur()).is_none());
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
        let s = compute("/sk", &[skill("create-pr")], &ctx(), &cur()).unwrap();
        let d = displays(&s);
        assert!(d.contains(&"/skill".to_string()));
        assert!(d.contains(&"/skin".to_string()));
        // "sk" does not match the skill "create-pr".
        assert!(!d.contains(&"create-pr".to_string()));
    }

    #[test]
    fn top_level_includes_matching_skills() {
        let s = compute("/cre", &[skill("create-pr")], &ctx(), &cur()).unwrap();
        // Commands first, then skills (`build_top_level`). `/secret` is here
        // because the match is a substring one and "se<cre>t" contains "cre" —
        // matching how the menu really behaves, rather than asserting a list
        // that any new command with those letters would break.
        assert_eq!(
            displays(&s),
            vec!["/secret".to_string(), "create-pr".to_string()]
        );
    }

    #[test]
    fn empty_slash_shows_commands_and_skills() {
        let s = compute("/", &[skill("create-pr")], &ctx(), &cur()).unwrap();
        let d = displays(&s);
        assert!(d.contains(&"/mcp".to_string()));
        assert!(d.contains(&"create-pr".to_string()));
    }

    #[test]
    fn panel_subcommands() {
        let s = compute("/panel ", &[], &ctx(), &cur()).unwrap();
        assert!(s.items.iter().any(|c| c.insert == "/panel preview "));
        assert_eq!(s.items.len(), 6);
    }

    #[test]
    fn descends_to_subcommands_after_space() {
        let s = compute("/mcp ", &[], &ctx(), &cur()).unwrap();
        let d = displays(&s);
        assert!(d.contains(&"list".to_string()));
        assert!(d.contains(&"add-remote".to_string()));
        let add = s.items.iter().find(|c| c.display == "list").unwrap();
        assert_eq!(add.insert, "/mcp list ");
        assert!(!add.has_children);
    }

    #[test]
    fn subcommands_filter_by_query() {
        let s = compute("/mcp add", &[], &ctx(), &cur()).unwrap();
        let d = displays(&s);
        assert!(d.contains(&"add".to_string()));
        assert!(d.contains(&"add-remote".to_string()));
        assert!(!d.contains(&"list".to_string()));
    }

    #[test]
    fn no_menu_past_layer_two() {
        assert!(compute("/mcp add foo", &[], &ctx(), &cur()).is_none());
    }

    #[test]
    fn command_without_subcommands_has_no_layer_two() {
        assert!(compute("/help ", &[], &ctx(), &cur()).is_none());
    }

    #[test]
    fn unknown_command_no_match_closes_menu() {
        assert!(compute("/zzz", &[], &ctx(), &cur()).is_none());
    }

    #[test]
    fn top_level_command_marks_children() {
        let s = compute("/mc", &[], &ctx(), &cur()).unwrap();
        let mcp = s.items.iter().find(|c| c.display == "/mcp").unwrap();
        assert!(mcp.has_children);
        assert_eq!(mcp.insert, "/mcp ");
        let help = compute("/hel", &[], &ctx(), &cur()).unwrap();
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
        assert!(compute("/mcp\nlist", &[], &ctx(), &cur()).is_none());
    }
    /// A missing agent reads nothing and must not panic: the menu degrades to
    /// its command layer rather than taking the session down.
    #[test]
    fn menu_context_is_fail_soft_on_a_missing_agent() {
        let home = tempfile::tempdir().unwrap();
        let ctx = MenuContext::load(home.path(), "nope");
        assert!(ctx.effort.is_empty());
        assert!(ctx.secrets.is_empty());
        assert!(ctx.notes.is_empty());
    }

    /// The levels are an arbitrary subset per model, never a prefix of one
    /// scale. A hardcoded low/medium/high/xhigh/max would be wrong for every
    /// row below except the first.
    #[test]
    fn effort_levels_follow_the_model_not_a_fixed_scale() {
        let levels = |model: &str| -> Vec<String> {
            compute("/effort ", &[], &effort_ctx(model), &cur())
                .map(|s| s.items.iter().map(|c| c.display.clone()).collect())
                .unwrap_or_default()
        };

        assert_eq!(levels("claude-opus-5").len(), 5);
        assert!(levels("claude-opus-5").contains(&"xhigh".to_string()));

        // 4.6 predates the xhigh step but keeps max.
        let opus46 = levels("claude-opus-4-6");
        assert!(!opus46.contains(&"xhigh".to_string()), "{opus46:?}");
        assert!(opus46.contains(&"max".to_string()), "{opus46:?}");

        // DeepSeek V4 publishes low/high/max — there is no medium.
        let ds = levels("deepseek-v4");
        assert!(!ds.contains(&"medium".to_string()), "{ds:?}");

        // A switch has two positions, not three that collapse to two.
        assert_eq!(levels("qwen3-32b").len(), 2);

        // gpt-5 and friends stop at high.
        assert_eq!(levels("gpt-5"), vec!["low", "medium", "high"]);
    }

    /// A model that rejects the parameter (Magistral, HTTP 422) or has no
    /// reasoning control (gpt-4o) opens no menu at all — and `/effort` still
    /// carries no marker promising one.
    #[test]
    fn a_model_without_effort_opens_no_menu_and_promises_none() {
        for model in ["magistral-medium-latest", "gpt-4o"] {
            let c = effort_ctx(model);
            assert!(c.effort.is_empty(), "{model}");
            assert!(compute("/effort ", &[], &c, &cur()).is_none(), "{model}");
            let top = compute("/effort", &[], &c, &cur()).unwrap();
            let row = top.items.iter().find(|i| i.display == "/effort").unwrap();
            assert!(!row.has_children, "{model} promised a layer it cannot open");
        }
    }

    /// `/secret` offers the KEYs already held plus the revoke flag; a new KEY
    /// is typed freely and simply matches nothing, which closes the menu.
    #[test]
    fn secret_offers_held_keys_and_delete() {
        let s = compute("/secret ", &[], &ctx(), &cur()).unwrap();
        let d: Vec<String> = s.items.iter().map(|c| c.display.clone()).collect();
        assert!(d.contains(&"GITHUB_TOKEN".to_string()), "{d:?}");
        assert!(d.contains(&"--delete".to_string()), "{d:?}");
        assert!(compute("/secret NEW_KEY", &[], &ctx(), &cur()).is_none());
    }

    /// `/model` completes registry aliases, described by the id behind them.
    #[test]
    fn model_offers_registry_aliases() {
        let s = compute("/model ", &[], &ctx(), &cur()).unwrap();
        let row = s.items.iter().find(|c| c.display == "fast").unwrap();
        assert_eq!(row.insert, "/model fast ");
        assert_eq!(row.desc, "deepseek-v4");
    }

    /// `/forget` completes note names, `last` first.
    #[test]
    fn forget_offers_last_then_note_names() {
        let s = compute("/forget ", &[], &ctx(), &cur()).unwrap();
        assert_eq!(s.items[0].display, "last");
        assert_eq!(s.items.len(), 2);
    }

    /// After a `/model` switch the menu must offer the NEW model's levels.
    /// Two shapes with different level counts, so a stale context cannot pass
    /// by coincidence.
    #[test]
    fn switching_models_changes_the_levels_on_offer() {
        let five = effort_ctx("claude-opus-5");
        let three = effort_ctx("gpt-5");
        assert_ne!(five.effort, three.effort);
        assert_eq!(
            compute("/effort ", &[], &five, &cur()).unwrap().items.len(),
            5
        );
        assert_eq!(
            compute("/effort ", &[], &three, &cur())
                .unwrap()
                .items
                .len(),
            3
        );
    }
}
