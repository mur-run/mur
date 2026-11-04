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

/// Every `--profile` value in the entry's launch argv, in argv order.
/// Accepts both spellings Clap's long option takes (`--profile <site>` and
/// `--profile=<site>`) and stops at `--`, after which tokens are positional.
/// One entry may carry several profiles (#2263); a legacy entry carries one.
pub(crate) fn attached_profiles(entry: &McpServerEntry) -> Vec<&str> {
    let mut out = Vec::new();
    let mut args = entry.args.iter().take_while(|a| *a != "--");
    while let Some(a) = args.next() {
        let Some(rest) = a.strip_prefix(PROFILE_OPT) else {
            continue;
        };
        if rest.is_empty() {
            if let Some(site) = args.next() {
                out.push(site.as_str());
            }
        } else if let Some(site) = rest.strip_prefix('=')
            && !site.is_empty()
        {
            out.push(site);
        }
    }
    out
}

/// Make `sites` exactly the profiles the entry launches with: every existing
/// `--profile` (either spelling) before any `--` is dropped and `--profile
/// <site>` pairs are written where the first one stood, or before `--` when
/// there was none. Nothing else changes, and nothing past `--` is touched.
pub(crate) fn set_profiles(entry: &mut McpServerEntry, sites: &[String]) {
    let args = &mut entry.args;
    let end = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let eq = format!("{PROFILE_OPT}=");
    let mut kept: Vec<String> = Vec::with_capacity(args.len() + 2 * sites.len());
    let mut at: Option<usize> = None;
    let mut i = 0;
    while i < end {
        let a = &args[i];
        if a == PROFILE_OPT {
            at.get_or_insert(kept.len());
            i += 2;
            continue;
        }
        if a.starts_with(&eq) {
            at.get_or_insert(kept.len());
            i += 1;
            continue;
        }
        kept.push(a.clone());
        i += 1;
    }
    let at = at.unwrap_or(kept.len());
    let flags = sites
        .iter()
        .flat_map(|s| [PROFILE_OPT.to_owned(), s.clone()]);
    kept.splice(at..at, flags);
    kept.extend(args.drain(end.min(args.len())..));
    *args = kept;
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
    /// [`LiveOps::save`], but only if the profile's MCP entries still equal
    /// `expected` — the snapshot the user confirmed. `Ok(None)` means another
    /// writer changed them since, and nothing was written. The comparison and
    /// the write happen under the same per-agent profile lock, so a confirmed
    /// snapshot can never overwrite a newer policy (#2204 review).
    fn save_if_unchanged(
        &mut self,
        expected: &[McpServerEntry],
        servers: Vec<McpServerEntry>,
    ) -> anyhow::Result<Option<(Vec<String>, Effect)>>;
    /// The notes [`LiveOps::save`] would report for `servers`, computed
    /// without writing — so a preview can tell "unchanged" from "needs repair".
    fn pending_repairs(&mut self, servers: &[McpServerEntry]) -> anyhow::Result<Vec<String>>;
    /// Append the `broad_audited_enabled` trace event after a `--research` write.
    fn record_research(&mut self, agent: &str) -> anyhow::Result<()>;
    /// Append the `profile_bound` trace event after a binder write (#2199).
    /// `old` is the profile the write replaced; `None` when `new` was added
    /// alongside the existing ones or there were none (#2263).
    fn record_profile_bound(
        &mut self,
        agent: &str,
        old: Option<&str>,
        new: &str,
    ) -> anyhow::Result<()>;
}
