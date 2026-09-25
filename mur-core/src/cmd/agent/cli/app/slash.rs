//! Slash-command parsing, moved out of `app/mod.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: every item below is verbatim.

/// A parsed slash command.
#[derive(Debug, PartialEq, Eq)]
pub enum SlashCmd {
    Help,
    Clear,
    Card,
    Sessions,
    /// `/channels [N] [--follow]` — list channels, switch to channel N, or
    /// live-tail channel N (`--follow` with no N stops following).
    Channels {
        n: Option<usize>,
        follow: bool,
    },
    /// `/auto [on|off]` — toggle (None) or set session-wide auto-approval.
    Auto(Option<bool>),
    /// `/verbose [on|off]` — toggle (None) or set expanded tool-card rendering.
    Verbose(Option<bool>),
    /// `/mcp [list|add|remove] …` — manage the agent's MCP servers.
    Mcp(Vec<String>),
    /// `/skill [list|add|remove] …` — manage the agent's skills.
    Skill(Vec<String>),
    /// `/remember [--kind rule|fact] <text>` — save an agent-local memory note.
    Remember(Vec<String>),
    /// `/memories` — list every note this agent can see, labeled by scope.
    Memories,
    /// `/forget <name|last>` — demote an agent-local note to Destroyed.
    Forget(Option<String>),
    /// `/skin [dark|light|mur]` — show or switch the active skin (persists to config).
    Skin(Option<String>),
    /// `/panel [tab] [target]` — open/drive the MUR Hub companion window.
    Panel(Vec<String>),
    /// `/browser [--add|auth|testing|automation]` — browser skill hub.
    /// `--add` attaches the skill to this agent (D6, reuses `/skill add`'s
    /// own `cmd_skill_add` path unmodified). A mode argument, once attached,
    /// is sent to the model directly as a turn — there is no `Unknown`
    /// fallthrough to `matched_skill` once this is a typed variant (D5).
    Browser(Vec<String>),
    /// `/open` — what is still outstanding, observed and reported kept apart.
    Open,
    /// `/deep-research [ask <question>|<question>|status|stop|setup]` — research
    /// fleet control. Aliased as `/research`; bare text is the question.
    DeepResearch(Vec<String>),
    /// `/search <query> [--all] [--limit N] [--send]` — search indexed project code.
    Search(Vec<String>),
    /// `/monitor` (or `/mon`) — list durable monitors (Task 13's `mur monitor
    /// list` rows), printed into the scrollback. Same handler as `Ctrl+T` /
    /// `Alt+M`.
    Monitor(Vec<String>),
    /// `/model [N|name]` — list registry models, or hot-switch to one.
    Model(Option<String>),
    /// `/effort [level] [--save]` — list the levels this model accepts, or set
    /// one. Session-scoped unless `--save` also writes the profile.
    Effort {
        level: Option<String>,
        save: bool,
    },
    /// `/login [anthropic|chatgpt]` — show OAuth health, or repair one provider.
    /// Unrelated to `mur auth login`, which signs in to mur.run.
    Login(Option<String>),
    /// `/secret <KEY> [--delete]` — hand the agent a credential through a
    /// hidden prompt, or revoke one. Only the KEY is ever on this line: a
    /// value typed here would be in the composer, the history, and the
    /// channel, which is the whole thing this command exists to avoid.
    Secret {
        key: Option<String>,
        delete: bool,
    },
    Quit,
    Unknown(String),
}

/// Parse a leading-slash command. Returns `None` for ordinary chat input.
pub fn parse_slash(line: &str) -> Option<SlashCmd> {
    let line = line.trim();
    let rest = line.strip_prefix('/')?;
    let mut words = rest.split_whitespace();
    let word = words.next().unwrap_or("");
    Some(match word {
        "help" | "h" | "?" => SlashCmd::Help,
        "clear" | "new" => SlashCmd::Clear,
        "card" => SlashCmd::Card,
        "sessions" | "ls" => SlashCmd::Sessions,
        "channels" | "chan" => {
            let args: Vec<&str> = words.collect();
            SlashCmd::Channels {
                n: args.iter().find_map(|s| s.parse::<usize>().ok()),
                follow: args.iter().any(|s| *s == "--follow" || *s == "-f"),
            }
        }
        "model" => SlashCmd::Model(words.next().map(str::to_string)),
        "effort" => {
            let args: Vec<&str> = words.collect();
            SlashCmd::Effort {
                level: args
                    .iter()
                    .find(|s| !s.starts_with("--"))
                    .map(|s| (*s).to_string()),
                save: args.contains(&"--save"),
            }
        }
        "login" => SlashCmd::Login(words.next().map(str::to_string)),
        "secret" => {
            let args: Vec<&str> = words.collect();
            SlashCmd::Secret {
                key: args
                    .iter()
                    .find(|s| !s.starts_with("--"))
                    .map(|s| (*s).to_string()),
                delete: args.contains(&"--delete"),
            }
        }
        "auto" => SlashCmd::Auto(match words.next() {
            Some("on") => Some(true),
            Some("off") => Some(false),
            _ => None,
        }),
        "verbose" => SlashCmd::Verbose(match words.next() {
            Some("on") => Some(true),
            Some("off") => Some(false),
            _ => None,
        }),
        "mcp" => SlashCmd::Mcp(words.map(str::to_string).collect()),
        "skill" | "skills" => SlashCmd::Skill(words.map(str::to_string).collect()),
        "remember" => SlashCmd::Remember(words.map(str::to_string).collect()),
        "memories" | "mem" => SlashCmd::Memories,
        "forget" => SlashCmd::Forget(words.next().map(str::to_string)),
        "skin" | "theme" => SlashCmd::Skin(words.next().map(str::to_string)),
        "panel" => SlashCmd::Panel(words.map(str::to_string).collect()),
        "browser" => SlashCmd::Browser(words.map(str::to_string).collect()),
        "deep-research" | "research" => SlashCmd::DeepResearch(words.map(str::to_string).collect()),
        "search" => SlashCmd::Search(words.map(str::to_string).collect()),
        "monitor" | "mon" => SlashCmd::Monitor(words.map(str::to_string).collect()),
        "open" | "todo" => SlashCmd::Open,
        "exit" | "quit" | "q" => SlashCmd::Quit,
        other => SlashCmd::Unknown(other.to_string()),
    })
}
