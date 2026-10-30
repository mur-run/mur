//! The live `browser` MCP entry as both `/browser live` (agent CLI) and
//! `mur browser` (top-level CLI) see it: its name, the saved profile its
//! launch argv attaches, host matching helpers, and the one write path
//! ([`LiveOps`]). Shared so a profile binder under `cmd::browser` (#2199)
//! writes through the same seam as `/browser live`, never around it.

use mur_common::agent::{McpServerEntry, McpServerNetwork};

use super::live_effect::Effect;

/// Profile entry name for the live browser server.
pub(crate) const LIVE_ENTRY: &str = "browser";

/// Long option the live browser's launch argv attaches a saved profile with.
const PROFILE_OPT: &str = "--profile";

/// The `--profile` value in the entry's launch argv, if any. Accepts both
/// spellings Clap's long option takes (`--profile <site>` and
/// `--profile=<site>`) and stops at `--`, after which tokens are positional.
pub(crate) fn attached_profile(entry: &McpServerEntry) -> Option<&str> {
    let mut args = entry.args.iter().take_while(|a| *a != "--");
    while let Some(a) = args.next() {
        let Some(rest) = a.strip_prefix(PROFILE_OPT) else {
            continue;
        };
        if rest.is_empty() {
            return args.next().map(String::as_str);
        }
        if let Some(site) = rest.strip_prefix('=') {
            return (!site.is_empty()).then_some(site);
        }
    }
    None
}

/// Point the entry's `--profile` at `site`, in place and in whichever
/// spelling the argv already uses (`--profile <site>` or `--profile=<site>`);
/// otherwise insert `--profile <site>` before any `--`. Never leaves a second
/// `--profile` behind, which Clap rejects at launch. Nothing else changes.
pub(crate) fn set_profile(entry: &mut McpServerEntry, site: &str) {
    let args = &mut entry.args;
    let end = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let eq = format!("{PROFILE_OPT}=");
    match args[..end]
        .iter()
        .position(|a| a == PROFILE_OPT || a.starts_with(&eq))
    {
        Some(i) if args[i] != PROFILE_OPT => args[i] = format!("{eq}{site}"),
        Some(i) if i + 1 < end => args[i + 1] = site.to_owned(),
        Some(i) => args.insert(i + 1, site.to_owned()),
        None => {
            args.splice(end..end, [PROFILE_OPT.to_owned(), site.to_owned()]);
        }
    }
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
    /// Append the `profile_bound` trace event after a binder write (#2199).
    fn record_profile_bound(
        &mut self,
        agent: &str,
        old: Option<&str>,
        new: &str,
    ) -> anyhow::Result<()>;
}
