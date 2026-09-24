//! Slash-command parsing, moved out of `app/mod.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: every item below is verbatim.

/// How the user named a channel on a `/channels` line.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ChannelRef {
    /// The stable number from the listing (`/channels 2`).
    Ordinal(u64),
    /// A channel-id prefix (`/channels 01a0d420`, or `#12345678` to force id
    /// matching when the prefix is all digits).
    IdPrefix(String),
}

/// Read one `/channels` argument as an ordinal or an id prefix. Bare digits
/// are an ordinal — ordinals are short and typed constantly, so they win the
/// ambiguity; `#` escapes to the id for the rare all-numeric prefix.
pub fn parse_channel_ref(arg: &str) -> Option<ChannelRef> {
    if let Some(id) = arg.strip_prefix('#') {
        return (!id.is_empty()).then(|| ChannelRef::IdPrefix(id.to_ascii_lowercase()));
    }
    if let Ok(n) = arg.parse::<u64>() {
        return Some(ChannelRef::Ordinal(n));
    }
    arg.chars()
        .all(|c| c.is_ascii_hexdigit())
        .then(|| ChannelRef::IdPrefix(arg.to_ascii_lowercase()))
}

/// A parsed slash command.
#[derive(Debug, PartialEq, Eq)]
pub enum SlashCmd {
    Help,
    Clear,
    Card,
    Sessions,
    /// `/channels [N|<id-prefix>] [--follow]` — list channels, switch to one,
    /// or live-tail it (`--follow` with no target stops following).
    ///
    /// Two ways to name a channel: the stable ordinal shown in the listing
    /// (`/channels 2`) or a channel-id prefix (`/channels 01a0d420`). Bare
    /// digits are always read as an ordinal; a `#` prefix forces id matching
    /// for the rare all-numeric id (`/channels #12345678`).
    Channels {
        target: Option<ChannelRef>,
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
                target: args
                    .iter()
                    .find(|s| !s.starts_with('-'))
                    .and_then(|s| parse_channel_ref(s)),
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
        "deep-research" | "research" => SlashCmd::DeepResearch(words.map(str::to_string).collect()),
        "search" => SlashCmd::Search(words.map(str::to_string).collect()),
        "monitor" | "mon" => SlashCmd::Monitor(words.map(str::to_string).collect()),
        "open" | "todo" => SlashCmd::Open,
        "exit" | "quit" | "q" => SlashCmd::Quit,
        other => SlashCmd::Unknown(other.to_string()),
    })
}
