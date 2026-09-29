//! Agent-proposed commands and actions — the `propose` tool's shared model.
//!
//! An agent that needs the user to run something calls `propose`; murmur shows
//! it as a chip under the composer. Two families:
//!
//! - **insert-only** (`shell`, `slash`): `Tab` puts the text into an empty
//!   composer; the user reviews and sends it. Never sent by a single key.
//! - **executable** (`restart`): `Enter` on an empty, idle composer runs it.
//!
//! [`vet`] is the one gate. The runtime calls it inside the tool's `execute`
//! (a rejection goes back to the model as a tool error) and murmur calls it on
//! the streamed args (so an arg the runtime would reject is never rendered).
//! It is pure — same input, same verdict on both sides.
//!
//! It lives here because `mur-agent-runtime` must not depend on `mur-core`,
//! and both of them need the type. No I/O.
//!
//! `restart` takes no target: it always means "restart the agent that
//! proposed it", so a proposal aimed at another agent cannot be expressed.
//!
//! Design: `docs/superpowers/specs/2026-09-28-murmur-proposal-chip-design.md`.

use std::fmt;

/// Canonical tool name. Shared by the runtime executor and the TUI interceptor.
pub const PROPOSE_TOOL: &str = "propose";

/// Longest label accepted, in chars. The chip is one line under the composer.
pub const LABEL_MAX_CHARS: usize = 80;

/// Longest insert-only command accepted, in chars.
pub const COMMAND_MAX_CHARS: usize = 500;

/// Wire values of the `kind` argument.
pub const KIND_SHELL: &str = "shell";
pub const KIND_SLASH: &str = "slash";
pub const KIND_RESTART: &str = "restart";

/// Allowlist of `kind` values, in schema order.
pub const KINDS: [&str; 3] = [KIND_SHELL, KIND_SLASH, KIND_RESTART];

/// What a proposal does when the user accepts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalKind {
    /// Shell command, inserted as `!<cmd>` (murmur's shell mode). Insert-only.
    Shell(String),
    /// Slash command, inserted as `/<cmd>`. Insert-only.
    Slash(String),
    /// Restart the proposing agent. Executable. No target by construction.
    Restart,
    /// A single suggested reply (murmur's ghost), inserted verbatim.
    /// Insert-only. Never built by [`vet`] — `reply` is not in [`KINDS`];
    /// only murmur's `suggest_replies` reveal constructs it.
    Reply(String),
}

/// A vetted proposal. Only [`vet`] builds one from untrusted args.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub label: String,
    pub kind: ProposalKind,
}

impl Proposal {
    /// Restart proposal with a caller-supplied label — for murmur's own hints
    /// (the `RESTART_HINT` sites), which are trusted, not model output.
    pub fn restart(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            kind: ProposalKind::Restart,
        }
    }

    /// A suggested reply shown as the composer's ghost text. Insert-only.
    pub fn reply(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            label: text.clone(),
            kind: ProposalKind::Reply(text),
        }
    }

    /// Whether this is a suggested reply (murmur's ghost).
    pub fn is_reply(&self) -> bool {
        matches!(self.kind, ProposalKind::Reply(_))
    }

    /// Whether `Enter` may run this proposal. Only native actions qualify;
    /// insert-only kinds never run on a single key (spec principle C).
    pub fn is_executable(&self) -> bool {
        matches!(self.kind, ProposalKind::Restart)
    }

    /// Text `Tab` puts into an empty composer, or `None` for executable kinds.
    pub fn insert_text(&self) -> Option<String> {
        match &self.kind {
            ProposalKind::Shell(cmd) => Some(format!("!{cmd}")),
            ProposalKind::Slash(cmd) => Some(format!("/{cmd}")),
            ProposalKind::Reply(text) => Some(text.clone()),
            ProposalKind::Restart => None,
        }
    }
}

/// Why a proposal was rejected. `Display` is the tool error the model reads,
/// so every message says what to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VetError {
    NotAnObject,
    MissingField(&'static str),
    UnknownKind(String),
    EmptyField(&'static str),
    TooLong { field: &'static str, max: usize },
    ControlChar(&'static str),
    Placeholder { field: &'static str, token: String },
    SecretShaped(&'static str),
    RestartTakesNoCommand,
}

impl fmt::Display for VetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAnObject => write!(f, "propose: arguments must be a JSON object"),
            Self::MissingField(k) => write!(f, "propose: `{k}` is required"),
            Self::UnknownKind(k) => write!(
                f,
                "propose: kind `{k}` is not allowed; use one of {}",
                KINDS.join(", ")
            ),
            Self::EmptyField(k) => write!(f, "propose: `{k}` must not be empty"),
            Self::TooLong { field, max } => {
                write!(f, "propose: `{field}` is longer than {max} characters")
            }
            Self::ControlChar(k) => write!(
                f,
                "propose: `{k}` contains a newline or control character; propose one single-line command"
            ),
            Self::Placeholder { field, token } => write!(
                f,
                "propose: `{field}` contains the placeholder `{token}`; write the concrete value"
            ),
            Self::SecretShaped(k) => write!(
                f,
                "propose: `{k}` looks like it contains a secret; never put credentials in a proposal"
            ),
            Self::RestartTakesNoCommand => write!(
                f,
                "propose: `restart` takes no `command`; it always restarts you, the proposing agent"
            ),
        }
    }
}

impl std::error::Error for VetError {}

/// Vet raw `propose` tool args into a [`Proposal`].
///
/// Rejects: missing/empty/over-long fields, kinds outside the allowlist,
/// newlines or control characters, unresolved placeholders (`<name>`,
/// `{agent}`, `{{x}}`), secret-shaped strings, and a `command` on `restart`.
pub fn vet(args: &serde_json::Value) -> Result<Proposal, VetError> {
    let obj = args.as_object().ok_or(VetError::NotAnObject)?;
    let label = str_field(obj, "label")?.ok_or(VetError::MissingField("label"))?;
    let kind = str_field(obj, "kind")?.ok_or(VetError::MissingField("kind"))?;
    let command = str_field(obj, "command")?;

    check_text("label", label, LABEL_MAX_CHARS)?;

    let kind = match kind {
        KIND_RESTART => {
            if command.is_some() {
                return Err(VetError::RestartTakesNoCommand);
            }
            ProposalKind::Restart
        }
        KIND_SHELL | KIND_SLASH => {
            let raw = command.ok_or(VetError::MissingField("command"))?;
            // Check the raw string: `trim()` below would silently drop a
            // trailing `\n`/`\r` and let it through.
            if raw.chars().any(char::is_control) {
                return Err(VetError::ControlChar("command"));
            }
            // Accept the prefix the model may add; store the bare command.
            let prefix = if kind == KIND_SHELL { '!' } else { '/' };
            let cmd = raw.trim().strip_prefix(prefix).unwrap_or(raw.trim()).trim();
            check_text("command", cmd, COMMAND_MAX_CHARS)?;
            if kind == KIND_SHELL {
                ProposalKind::Shell(cmd.to_string())
            } else {
                ProposalKind::Slash(cmd.to_string())
            }
        }
        other => return Err(VetError::UnknownKind(other.to_string())),
    };

    Ok(Proposal {
        label: label.trim().to_string(),
        kind,
    })
}

fn str_field<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    key: &'static str,
) -> Result<Option<&'a str>, VetError> {
    match obj.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(VetError::MissingField(key)),
    }
}

fn check_text(field: &'static str, s: &str, max: usize) -> Result<(), VetError> {
    if s.chars().any(char::is_control) {
        return Err(VetError::ControlChar(field));
    }
    if s.trim().is_empty() {
        return Err(VetError::EmptyField(field));
    }
    if s.chars().count() > max {
        return Err(VetError::TooLong { field, max });
    }
    if let Some(token) = find_placeholder(s) {
        return Err(VetError::Placeholder { field, token });
    }
    if matches!(crate::redact::redact_secrets(s), std::borrow::Cow::Owned(_)) {
        return Err(VetError::SecretShaped(field));
    }
    Ok(())
}

/// First template-style placeholder in `s`: `<ident>`, `{ident}`, `{{ident}}`.
/// `${VAR}` is a shell expansion, not a placeholder, and is left alone.
fn find_placeholder(s: &str) -> Option<String> {
    use regex_lite::Regex;
    use std::sync::OnceLock;
    static RX: OnceLock<Regex> = OnceLock::new();
    let rx = RX.get_or_init(|| {
        Regex::new(r"<[A-Za-z_][A-Za-z0-9_-]*>|(^|[^$])(\{\{?[A-Za-z_][A-Za-z0-9_-]*\}\}?)")
            .expect("static placeholder regex")
    });
    let caps = rx.captures(s)?;
    let m = caps.get(2).or_else(|| caps.get(0))?;
    Some(m.as_str().to_string())
}

#[cfg(test)]
#[path = "proposal_tests.rs"]
mod tests;
