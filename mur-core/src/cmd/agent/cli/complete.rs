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
use super::theme::SKIN_CHOICES;
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
const BROWSER_MODES: &[(&str, &str)] = &[
    ("--add", "attach the browser skill to this agent"),
    ("auth", "log in once, keep an encrypted profile"),
    ("testing", "record/replay an end-to-end test"),
    ("automation", "run a repeatable browser task"),
];
/// `/search` takes free text, so the menu can only teach the flags — the
/// query itself is typed, not completed.
const SEARCH_FLAGS: &[(&str, &str)] = &[
    ("--all", "search every indexed project"),
    ("--limit", "cap the number of hits"),
    ("--lines", "preview lines per hit (default 3)"),
    ("--expand", "show full content for hit ids, e.g. 3,7"),
    ("--send", "send the results to the agent"),
];

const DEEP_RESEARCH_SUBS: &[(&str, &str)] = &[
    // `ask` is offered even though bare text works without it: the menu is
    // the only place a user learns the verb exists, and a menu that lists
    // only status/stop/setup reads as "asking is not one of the options".
    ("ask", "research a question"),
    ("status", "show the fleet panel"),
    ("stop", "write the kill-switch"),
    ("setup", "how to run the wizard"),
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
    (
        "browser",
        "browser skill hub — auth, testing, automation",
        Args::Fixed(BROWSER_MODES),
    ),
    ("card", "show this agent's card", Args::None),
    (
        "channels",
        "list, switch, or follow channels",
        Args::Fixed(CHANNELS_ARGS),
    ),
    ("clear", "start a new conversation", Args::None),
    (
        "deep-research",
        "run the research fleet, or show its status",
        Args::Fixed(DEEP_RESEARCH_SUBS),
    ),
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
    (
        "monitor",
        "list durable monitors that want attention",
        Args::None,
    ),
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
    (
        "search",
        "search indexed project code",
        Args::Fixed(SEARCH_FLAGS),
    ),
    ("sessions", "list past sessions", Args::None),
    ("skill", "manage agent skills", Args::Fixed(SKILL_SUBS)),
    ("skin", "switch theme", Args::Fixed(SKIN_CHOICES)),
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
            insert: if *word == "deep-research" {
                format!("/{word}")
            } else {
                format!("/{word} ")
            },
            desc: (*desc).to_string(),
            // Bare `/deep-research` means status and must be directly
            // sendable; a manually typed trailing space still opens layer 2.
            has_children: *word != "deep-research" && !build_args(word, *args, ctx).is_empty(),
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
#[path = "complete_tests.rs"]
mod tests;
