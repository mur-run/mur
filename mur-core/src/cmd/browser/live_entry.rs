//! The live `browser` MCP entry as both `/browser live` (agent CLI) and
//! `mur browser` (top-level CLI) see it: its name, the saved profile its
//! launch argv attaches, host matching helpers, and the one write path
//! ([`LiveOps`]). Shared so a profile binder under `cmd::browser` (#2199)
//! writes through the same seam as `/browser live`, never around it.

use mur_common::agent::{McpServerEntry, McpServerNetwork};

use super::live_effect::Effect;

/// Profile entry name for the live browser server.
pub(crate) const LIVE_ENTRY: &str = "browser";

/// The `--profile <site>` value in the entry's launch argv, if any.
pub(crate) fn attached_profile(entry: &McpServerEntry) -> Option<&str> {
    let i = entry.args.iter().position(|a| a == "--profile")?;
    entry.args.get(i + 1).map(String::as_str)
}

pub(crate) fn norm(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

/// `a` is `b` or a subdomain of it, label-aligned (`evilb.com` is not `b.com`).
pub(crate) fn within(a: &str, b: &str) -> bool {
    a == b || a.strip_suffix(b).is_some_and(|p| p.ends_with('.'))
}

/// Profile side effects, injected so tests run with a fake.
pub(crate) trait LiveOps {
    /// Current MCP entries on the agent.
    fn servers(&mut self) -> anyhow::Result<Vec<McpServerEntry>>;
    /// Create the entry WITH its network policy through
    /// `manage::mcp_add_with_network` (pins + probes behind the egress proxy +
    /// one atomic save). `Err` means nothing was written.
    fn mcp_add(&mut self, argv: &[String], network: McpServerNetwork) -> anyhow::Result<String>;
    /// Persist `servers` back to the profile, together with any missing
    /// [`live_fs`](crate::cmd::agent::cli::browser_live_cmd::live_fs) grants and a [`repin_command`](crate::cmd::agent::cli::browser_live_cmd::repin_command) onto this binary's absolute
    /// path (an entry made before those existed lacks both).
    /// Returns one note per grant added, and when the write reaches the
    /// running agent. `Err` means nothing was written.
    fn save(&mut self, servers: Vec<McpServerEntry>) -> anyhow::Result<(Vec<String>, Effect)>;
    /// The notes [`LiveOps::save`] would report for `servers`, computed
    /// without writing — so a preview can tell "unchanged" from "needs repair".
    fn pending_repairs(&mut self, servers: &[McpServerEntry]) -> anyhow::Result<Vec<String>>;
    /// Append the `broad_audited_enabled` trace event after a `--research` write.
    fn record_research(&mut self, agent: &str) -> anyhow::Result<()>;
}
