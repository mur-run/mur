//! #008 layer 2: what tier is this tool call?
//!
//! The cli's session lane (`/auto`, `--auto`, the per-tool `[a]` grant, and
//! since #1228 the DEFAULT session) is standing, unattended authority. The
//! ceiling on that authority already exists and already has its reasoning
//! written down — `crate::hitl::tier_may_be_granted`, capped at `Write`:
//!
//! > `Spend`, `Destructive` and `Privileged` are exactly the actions whose
//! > cost a human cannot undo by noticing later, and `NetworkEgress` is how
//! > data leaves — none of them belongs behind a config line today.
//!
//! Gate A (`mur-core/src/hitl/gate.rs:82`) consults it. Gate B — the chat
//! gate the TUI drives — had no tier at all, so the ceiling could not bind it.
//! This module supplies the missing input, and nothing more.
//!
//! ## What this is NOT
//!
//! Not a safety proof. It is a deny-list over command heads, and a deny-list
//! is only ever as good as its last audit. Trail of Bits (2025-10) showed
//! allowlisted commands driven past their classification by argument
//! injection; the same trick works on a head-matching deny-list. So:
//!
//! - **The fallback is `Write`, not `Read`.** An unknown head still counts as
//!   mutating, which keeps it inside the ceiling and auto-answered. Falling
//!   back to `Privileged` would make the #1228 default ask on every unfamiliar
//!   build command and train blind approval — the exact failure the read-lane
//!   comment in `stream_handler.rs` describes.
//! - **`Read` is never guessed.** It is delegated to `bash_class`, which
//!   already fails safe on shell metacharacters and has an audited head list
//!   with a "do NOT re-add without a full audit" note on it.
//! - **Metacharacters escalate.** `a; rm -rf b` is not classifiable by head,
//!   so anything carrying a chain/substitution operator that also names a
//!   high-tier word is treated as that high tier rather than parsed.
//!
//! This is layer 2 of three. Layer 1 is the sandbox/entitlement floor, which
//! is unaffected by any of this and denies regardless. Layer 3
//! (`crate::hitl::intent`) is a model that may raise a tier but never lower
//! one. It NARROWS this module's blind spot; it does not close it:
//!
//! - **It sees the command string only.** For `python3 sync.py` it reads
//!   those three words, never what `sync.py` does.
//! - **It sees only `bash` calls with a command whose tier here is exactly
//!   `Write`.** Non-`bash` tools, and calls that fall to `Write` without a
//!   command (the `_ => RiskTier::Write` arm of `classify`), are never asked.
//! - **A wrong `Read` is out of its reach by design.** A call misclassified as
//!   `Read` is fixed in `bash_class` / `dest`, never by widening
//!   `intent::should_check`. Once a static misclassification is shown, it is
//!   fixed here; that the model sometimes catches it does not count as fixed.
//! - **It is fail-open and off by default.** No answer, a timeout or an error
//!   keeps the tier from this module, and no profile has it unless asked. No
//!   safety property may depend on it.

use crate::hitl::RiskTier;
use serde_json::Value;

use super::{bash_class, dest};

/// Heads that take a credential or change who you are.
const PRIVILEGED_HEADS: &[&str] = &[
    "sudo",
    "su",
    "doas",
    "chown",
    "chgrp",
    "chmod",
    "passwd",
    "ssh-keygen",
    "security",
    "keychain",
    "launchctl",
    "systemctl",
    "mount",
    "umount",
    "diskutil",
    "csrutil",
    "visudo",
    "dseditgroup",
    "pfctl",
    "iptables",
];

/// Heads whose ordinary use moves bytes off this machine.
const EGRESS_HEADS: &[&str] = &[
    "curl",
    "wget",
    "scp",
    "rsync",
    "sftp",
    "ftp",
    "nc",
    "ncat",
    "telnet",
    "ssh",
    "http",
    "httpie",
    "aws",
    "gcloud",
    "az",
    "kubectl",
    "docker",
    "gh",
    "glab",
    // Forge CLIs, same reason as `gh`: `tea pr create` publishes to a Gitea
    // instance. `tea` was missing here, so it fell through to the `Write`
    // fallback and the default session answered it with nobody in the loop.
    // Its READ verbs are held in the read lane by `bash_class`, which runs
    // first — the deny-list only ever sees what that list refused.
    "tea",
    // Sync/object-store clients: their ordinary use is uploading a local tree.
    "rclone",
    "s3cmd",
    "gsutil",
    "npm",
    "pnpm",
    "yarn",
    "pip",
    "pip3",
    "cargo-publish",
    "twine",
];

/// Heads that destroy or overwrite without a recovery path.
const DESTRUCTIVE_HEADS: &[&str] = &[
    "rm", "rmdir", "shred", "srm", "dd", "mkfs", "fdisk", "truncate", "killall", "pkill", "reboot",
    "shutdown", "halt",
];

/// Heads that spend money or dispatch paid work.
const SPEND_HEADS: &[&str] = &["mur-fleet", "terraform", "pulumi"];

/// Tool names that are spend/dispatch by nature, whatever their arguments.
/// Mirrors `NO_ALWAYS_TOOLS` in the GUI's `hitlModel.ts` — one list of
/// "never standing-granted" dispatch tools, two surfaces, and they must not
/// drift.
const SPEND_TOOLS: &[&str] = &["fleet_run", "parallel_jobs", "delegate_to"];

/// Substrings that make an otherwise-ordinary command irreversible. Matched
/// against the whole command because they are argument-level, not head-level.
const DESTRUCTIVE_ARGS: &[&str] = &[
    "--force",
    "-f origin",
    "--hard",
    "push --delete",
    "branch -D",
    "reflog expire",
    "gc --prune",
    "clean -fd",
    "--no-preserve-root",
];

/// `git` subcommands that publish, i.e. leave this machine.
///
/// INBOUND IS NOT EGRESS. `fetch` and `pull` used to sit here beside `push`,
/// and the cost was measured: `NetworkEgress` is above
/// `tier_may_be_granted`, so the approval menu's "don't ask again" row came
/// back `Refused` and `/auto` could not cover them either — every `git fetch`
/// of a session asked again, identically, and a prompt that is always
/// answered the same way is what trains blind approval. A fetch downloads;
/// nothing of the operator's is disclosed by it. They now fall to the `Write`
/// fallback, which is the honest tier (`pull` moves the working tree) and is
/// inside the ceiling. `clone` stays: it names a remote URL and writes a new
/// tree from it. `remote`/`submodule` stay: both have write modes that
/// repoint where a later push lands.
const GIT_EGRESS_SUBCMDS: &[&str] = &["push", "clone", "remote", "submodule"];

/// Shell operators that mean "more than one command" — the classifier cannot
/// reason past them, so their presence can only raise a tier, never lower it.
const CHAINING: &[char] = &[';', '|', '&', '`', '$', '>', '<', '\n'];

/// The tier of one tool call. Never LLM-asserted, never read from anything
/// the agent wrote — the same rule `mur-monitor`'s `action::risk::classify`
/// states for its own table.
/// #1600: the tier a gate decides on. The profile's declared `risk:` can only
/// RAISE the classifier's answer (most-restrictive-wins) — a `risk: read` on a
/// `rm -rf` must not drag it under the ceiling.
pub fn effective_tier(
    tool_name: &str,
    tool_input: Option<&Value>,
    declared: Option<RiskTier>,
) -> RiskTier {
    classify(tool_name, tool_input).max(declared.unwrap_or(RiskTier::Read))
}

/// Is this a spend/dispatch tool by nature? Matched on the leaf name, so an
/// MCP prefix (`mcp__mur-tools__parallel_jobs`) cannot hide one.
pub fn is_spend_tool(tool_name: &str) -> bool {
    SPEND_TOOLS.contains(&leaf(tool_name))
}

/// The tier floor a spend tool carries whatever it declares: `Some(Spend)` for
/// a spend tool, `None` otherwise. For callers that combine it with `max`.
pub fn spend_floor(tool_name: &str) -> Option<RiskTier> {
    is_spend_tool(tool_name).then_some(RiskTier::Spend)
}

pub fn classify(tool_name: &str, tool_input: Option<&Value>) -> RiskTier {
    classify_with_trigger(tool_name, tool_input).0
}

/// [`classify`] plus, for `bash`, the rule that set the tier (#2362) — what
/// the ceiling record names so a prompt can be explained afterwards without
/// the full command. `None` when no deny-list rule fired.
pub fn classify_with_trigger(
    tool_name: &str,
    tool_input: Option<&Value>,
) -> (RiskTier, Option<String>) {
    if is_spend_tool(tool_name) {
        return (RiskTier::Spend, Some(leaf(tool_name).to_string()));
    }
    if let Some(tier) = crate::hitl::builtin_tier::browser_tier(tool_name) {
        return (tier, None);
    }
    // Read is delegated, never guessed: `bash_class` fails safe on
    // metacharacters and its head list carries an audit note.
    if bash_class::is_readonly_call(tool_name, tool_input) {
        return (RiskTier::Read, None);
    }
    // A CHAIN of reads is still a read. `bash_class` refuses every operator,
    // so `cd repo && git status -sb | head -5` used to fall through to the
    // head deny-list below, where `git remote` or `gh` anywhere in the line
    // lifted the whole thing to `NetworkEgress` — a prompt, with a "can't
    // remember this" row, for a command that reads. `dest::classify_bash` is
    // the parser that already splits on those operators and proves EVERY
    // segment through the same audited list; a `Local` scope is that proof.
    // A remote scope stays where the ssh head puts it — a hop is not local.
    if tool_name == "bash"
        && let Some(cmd) = tool_input
            .and_then(|v| v.get("command"))
            .and_then(Value::as_str)
        && dest::classify_bash(cmd).is_some_and(|s| s.dest == dest::Destination::Local)
    {
        return (RiskTier::Read, None);
    }
    match tool_name {
        "bash" => tool_input
            .and_then(|v| v.get("command"))
            .and_then(Value::as_str)
            .map_or((RiskTier::Write, None), classify_bash_trigger),
        // A dedicated write tool is a write, by construction. Whether it may
        // reach a given path is layer 1's question, not this one's.
        "write_file" | "edit_file" => (RiskTier::Write, None),
        _ => (RiskTier::Write, None),
    }
}

/// `mcp__server__tool` → `tool`. An MCP server must not lower a tool's tier by
/// prefixing it.
fn leaf(name: &str) -> &str {
    name.rsplit("__").next().unwrap_or(name)
}

/// The tier of one `bash` command, plus the rule that set it (#2362), for the
/// ceiling record: a deny-listed word (`chmod`), a `git` subcommand
/// (`git push`), a dotted call (`os.chmod`), a destructive argument
/// (`--force`) or `opaque_shell`. `None` = the `Write` fallback.
fn classify_bash_trigger(cmd: &str) -> (RiskTier, Option<String>) {
    let cmd = cmd.trim();
    // Only the segments NOT proved read-only are scanned for heads: the
    // arguments of a proved read are data (`grep -c npm f` searches for the
    // string `npm`, it does not run it). When the parser cannot prove
    // anything — substitution, redirection, heredocs — the whole command is
    // scanned, except the bodies of heredocs written to a file: those are
    // data (`heredoc_data`). `opaque_shell` always sees the whole command.
    let scanned = dest::unproven_text(cmd)
        .unwrap_or_else(|| super::heredoc_data::scan_text(cmd).into_owned());
    let lower = scanned.to_ascii_lowercase();

    // Most-restrictive-wins: each check may only raise the tier, and the
    // trigger kept is the first one found at the final tier. Every check runs
    // against the WHOLE command, so a chained `a && sudo b` is caught by the
    // `sudo` check without the classifier having to parse the chain.
    let mut tier = RiskTier::Write;
    let mut trigger: Option<String> = None;
    let mut raise = |to: RiskTier, why: Option<String>| {
        if let Some(why) = why
            && to > tier
        {
            tier = to;
            trigger = Some(why);
        }
    };

    raise(RiskTier::Spend, first_word_in(&lower, SPEND_HEADS));
    raise(
        RiskTier::Destructive,
        first_word_in(&lower, DESTRUCTIVE_HEADS).or_else(|| {
            DESTRUCTIVE_ARGS
                .iter()
                .find(|a| lower.contains(*a))
                .map(|a| (*a).to_string())
        }),
    );
    // #1832: a body that cannot be read (`eval $X`, `sh -c "$X"`, `… | sh`) is
    // arbitrary code execution. A policy choice, narrow by design — see
    // `opaque_shell`.
    if super::opaque_shell::is_opaque_shell(cmd) {
        // Count only: the command text is never logged (it may hold secrets).
        // Enable with `RUST_LOG=mur_common::hitl::tool_tier=debug` to measure
        // how often this rule fires before deciding whether to keep it.
        tracing::debug!(
            target: "mur_common::hitl::tool_tier",
            rule = "opaque_shell",
            "escalating opaque shell invocation to Destructive"
        );
        raise(
            RiskTier::Destructive,
            Some(OPAQUE_SHELL_TRIGGER.to_string()),
        );
    }
    raise(
        RiskTier::NetworkEgress,
        first_word_in(&lower, EGRESS_HEADS)
            .or_else(|| git_sub_in(&lower, GIT_EGRESS_SUBCMDS).map(|s| format!("git {s}"))),
    );
    // #2362: a mode change on the agent's own files is not a privilege
    // change; those segments are left out of THIS scan only.
    let privileged_text = self_chmod::without_self_scoped(cmd, &scanned).to_ascii_lowercase();
    raise(
        RiskTier::Privileged,
        first_word_in(&privileged_text, PRIVILEGED_HEADS)
            .or_else(|| dotted::privileged_call(&privileged_text).map(str::to_string)),
    );
    (tier, trigger)
}

/// Trigger name recorded when `opaque_shell` set the tier.
const OPAQUE_SHELL_TRIGGER: &str = "opaque_shell";

/// The first of `words` that `cmd` contains as a whole token. Token-wise rather than
/// substring so `rm` does not fire on `cargo fmt --rm-nothing`, while still
/// catching it after a chain operator (`a && rm b`) — which is why the
/// splitter includes the chaining characters.
fn first_word_in(cmd: &str, words: &[&str]) -> Option<String> {
    tokens(cmd).find(|t| words.contains(t)).map(str::to_string)
}

/// #1841: command words, normalised so a head cannot hide from the deny-list.
///
/// - Quotes and parentheses are separators, so the body of `bash -c "sudo ls"`,
///   `$(sudo ls)` and `` `sudo ls` `` is seen as ordinary words (nesting
///   included) instead of one opaque quoted token.
/// - A path-qualified word compares by basename, so `/usr/bin/sudo` is `sudo`.
///
/// This can only RAISE a tier, never lower one: it adds matches and removes
/// none. The cost is a rare false positive (an argument path ending in a head's
/// name, e.g. `/tmp/rm`), which prompts instead of auto-answering. An opaque
/// body (`bash -c "$X"`, `eval`) is invisible here; `opaque_shell` (#1832)
/// handles those shapes separately.
fn tokens(cmd: &str) -> impl Iterator<Item = &str> {
    cmd.split(|c: char| {
        c.is_whitespace() || CHAINING.contains(&c) || matches!(c, '"' | '\'' | '(' | ')')
    })
    .filter(|t| !t.is_empty())
    .map(|t| t.rsplit('/').next().unwrap_or(t))
    .filter(|t| !t.is_empty())
}

fn git_sub_in(cmd: &str, subs: &[&str]) -> Option<String> {
    let toks: Vec<&str> = tokens(cmd).collect();
    toks.windows(2)
        .find(|w| w[0] == "git" && subs.contains(&w[1]))
        .map(|w| w[1].to_string())
}

mod dotted;
mod self_chmod;

#[cfg(test)]
mod readonly_regressions;

#[cfg(test)]
mod tests;
