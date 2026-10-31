//! `/browser live <host>...`: persist a live-mode browser MCP entry on this
//! agent and add the given hosts to its egress allowlist. `--replace` scopes
//! it to exactly the given hosts instead (#2101).
//!
//! Editing an existing entry (hosts, `--profile`) hot-reloads: the agent's
//! next turn uses it. Creating the entry, a new grant, or a research-mode
//! switch still needs a restart, offered as a chip and never run
//! automatically (it would end the very session that typed the command).
//! Which one a write was is decided in [`super::browser_live_effect`].
//!
//! The decision logic is pure ([`parse_hosts`], [`apply`]) so tests never
//! touch a profile on disk or spawn the install probe `manage::mcp_add` runs.

use mur_agent_runtime::sandbox::egress_proxy::DENY_OVERLAY_ENFORCEMENT;
use mur_agent_runtime::sandbox::policy::RESTRICTED_GENERAL_PORTS;
use std::path::Path;

use mur_common::agent::{FilesystemEntitlement, McpNetMode, McpServerEntry, McpServerNetwork};
use mur_common::host_pattern::{Normalization, normalize_host_pattern};
use mur_common::proposal::Proposal;

use super::browser_live_effect::Effect;
pub(crate) use super::browser_live_ops::ProfileOps;
use super::browser_live_preview::render_preview;
#[cfg(test)]
use super::manage;
use super::manage::Managed;

pub(super) use crate::cmd::browser::live_entry::{LIVE_ENTRY, LiveOps};

/// Example shown when no host was given.
pub(super) const USAGE_EXAMPLE: &str = "/browser live shop-a.example.com";

/// Where `/browser` should go for a given argument list.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Route {
    /// `--add`: attach the skill (unchanged legacy behavior).
    Add,
    /// `live ...`: the remaining args are the hosts.
    Live(Vec<String>),
    /// `allow ...`: grant one more host (#1907).
    Allow(Vec<String>),
    /// `status`: the effective network policy (#1906).
    Status,
    /// `list [keyword]... [--tag <t>]...`: recorded runs (#2101).
    List,
    /// Anything else: forwarded to the model as a browser-skill turn.
    Turn,
}

pub(super) fn route(args: &[String]) -> Route {
    match args.first().map(String::as_str) {
        Some("--add") => Route::Add,
        Some("live") => Route::Live(args[1..].to_vec()),
        Some("allow") => Route::Allow(args[1..].to_vec()),
        Some("status") if args.len() == 1 => Route::Status,
        Some("list") => Route::List,
        _ => Route::Turn,
    }
}

/// What the rerun says about the entry's saved profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProfileArg {
    /// Neither flag: a new entry starts sessionless, an existing one keeps
    /// whatever profile it has.
    Keep,
    /// `--profile <site>`: launch with that saved session.
    Set(String),
    /// `--no-profile`: drop the saved session (the undo for a first bind).
    Clear,
}

impl ProfileArg {
    fn site(&self) -> Option<&str> {
        match self {
            Self::Set(site) => Some(site),
            Self::Keep | Self::Clear => None,
        }
    }
}

/// Split `--profile <site>` / `--no-profile` out of the argument list,
/// leaving the hosts.
///
/// Without a profile the live browser launches with no cookies, so every
/// authenticated page redirects to its login form. That is a missing
/// `--storage-state` on the launch, not a missing browser application, so the
/// fix is naming the profile `mur browser auth` saved — never granting a
/// branded Chrome.
pub(super) fn split_profile(args: &[String]) -> Result<(Vec<String>, ProfileArg), String> {
    let mut hosts = Vec::new();
    let mut profile = None;
    let mut clear = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--profile" => {
                let site = it.next().ok_or_else(|| {
                    format!("`--profile` needs a saved profile name, e.g. `{USAGE_EXAMPLE} --profile my-site`")
                })?;
                if profile.replace(site.clone()).is_some() {
                    return Err("`--profile` was given twice; name one profile".to_owned());
                }
            }
            "--no-profile" => clear = true,
            _ => hosts.push(a.clone()),
        }
    }
    match (profile, clear) {
        (Some(_), true) => {
            Err("`--profile` and `--no-profile` contradict each other; give one".to_owned())
        }
        (Some(site), false) => {
            // Same charset the profile directory uses; a bad name must fail
            // here, before anything is written, not at launch inside the agent.
            mur_browser::paths::validate_name(&site)
                .map_err(|e| format!("`--profile {site}`: {e}"))?;
            Ok((hosts, ProfileArg::Set(site)))
        }
        (None, true) => Ok((hosts, ProfileArg::Clear)),
        (None, false) => Ok((hosts, ProfileArg::Keep)),
    }
}

/// Hosts that passed validation, plus display lines for every input that was
/// rewritten on the way (URL stripped, lowercased, punycoded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedHosts {
    pub hosts: Vec<String>,
    pub notes: Vec<String>,
}

/// Split `host[:port]` without cutting an IPv6 address at a colon: only
/// `[v6]:port` or an authority with exactly one `:` carries a port.
pub(super) fn split_authority(authority: &str) -> (&str, Option<&str>) {
    if authority.starts_with('[') {
        if let Some(i) = authority.find("]:") {
            return (&authority[..=i], Some(&authority[i + 2..]));
        }
        return (authority, None);
    }
    if authority.bytes().filter(|&b| b == b':').count() == 1 {
        let (h, p) = authority.rsplit_once(':').unwrap_or((authority, ""));
        return (h, Some(p));
    }
    (authority, None)
}

/// Validate and normalize hosts. `host:443` → `host`; a port outside the
/// restricted-mode web set is rejected (see #1619). Every argument that is not
/// a host is named in one refusal (#1905), and nothing is written unless all
/// of them pass. Error text is user-facing.
pub(super) fn parse_hosts(args: &[String]) -> Result<ParsedHosts, String> {
    if args.is_empty() {
        return Err(format!(
            "usage: /browser live <host>... — name the sites this agent may reach, e.g. `{USAGE_EXAMPLE}` \
             (or `/browser live --research` to allow every host, audited)"
        ));
    }
    let mut out = ParsedHosts {
        hosts: Vec::new(),
        notes: Vec::new(),
    };
    let mut bad: Vec<String> = Vec::new();
    for raw in args {
        let authority = strip_url(raw)?;
        let (host, port) = match split_authority(authority) {
            (_, Some("")) => {
                return Err(format!(
                    "`{raw}`: the port after `:` is empty — drop the colon or give a port"
                ));
            }
            (h, Some(p)) => {
                let port: u16 = p
                    .parse()
                    .map_err(|_| format!("`{raw}`: port is not a number"))?;
                (h, Some(port))
            }
            (h, None) => (h, None),
        };
        // `-` first: `/browser live` parses no flags of its own here, so
        // `--add` is a mistake, never a hostname to put in the allowlist.
        if host.starts_with('-') {
            bad.push(format!("`{raw}` (looks like a flag)"));
            continue;
        }
        if host.is_empty() {
            bad.push(format!("`{raw}` (empty host)"));
            continue;
        }
        if let Some(p) = port
            && !RESTRICTED_GENERAL_PORTS.contains(&p)
        {
            // #1619: auto-opening the port is unsafe until Chromium is shown
            // not to bypass the proxy, so hand the choice back to the user.
            return Err(format!(
                "port {p} is not in restricted mode's web set {RESTRICTED_GENERAL_PORTS:?}; \
                 nothing was changed. Open it yourself with `mur agent perm allow-port <agent> {p}` \
                 (see #1619), then rerun `/browser live {host}`"
            ));
        }
        let n = match normalize_host_pattern(host) {
            Ok(n) => n,
            Err(e) => {
                bad.push(format!("`{raw}` ({e})"));
                continue;
            }
        };
        if out.hosts.contains(&n.value) {
            continue;
        }
        if raw != &n.value {
            out.notes.push(format!("`{raw}` → `{}`", n.value));
        }
        for note in &n.notes {
            if let Normalization::Punycode { from } = note {
                out.notes
                    .push(format!("`{from}` is stored as punycode `{}`", n.value));
            }
        }
        out.hosts.push(n.value);
    }
    if !bad.is_empty() {
        return Err(format!(
            "not a host: {} — nothing was changed. Name sites like `{USAGE_EXAMPLE}`",
            bad.join("; ")
        ));
    }
    Ok(out)
}

/// Schemes `/browser live` accepts in a pasted URL (#1676).
const URL_SCHEMES: [&str; 2] = ["http", "https"];

/// Reduce URL-shaped input to its `host[:port]` (#1676): a pasted
/// `https://example.com:8443/path?q` becomes `example.com:8443`. Bare
/// `host[:port]` passes through unchanged; a non-web scheme is refused.
pub(super) fn strip_url(raw: &str) -> Result<&str, String> {
    let Some((scheme, rest)) = raw.split_once("://") else {
        return Ok(raw);
    };
    if !URL_SCHEMES.iter().any(|s| s.eq_ignore_ascii_case(scheme)) {
        return Err(format!(
            "`{raw}`: only http:// and https:// URLs are accepted — or pass a bare hostname, \
             e.g. `{USAGE_EXAMPLE}`"
        ));
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Ok(&rest[..end])
}

/// What [`apply`] changed, for the summary line.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Applied {
    /// The live entry was newly created (false = already present, reused).
    pub added: bool,
    /// The allowlist this call replaced (empty if none).
    pub previous: Vec<String>,
}

/// Ensure the live entry exists and set its allowlist to exactly `hosts`.
/// `new_entry` is only called when the entry is missing.
#[cfg(test)]
pub(super) fn apply(
    servers: &mut Vec<McpServerEntry>,
    hosts: &[String],
    new_entry: impl FnOnce() -> McpServerEntry,
) -> Applied {
    apply_network(servers, live_network(hosts), new_entry)
}

/// [`apply`] for any policy: `Restricted` hosts or `--research`'s `BroadAudited`.
pub(super) fn apply_network(
    servers: &mut Vec<McpServerEntry>,
    network: McpServerNetwork,
    new_entry: impl FnOnce() -> McpServerEntry,
) -> Applied {
    let added = !servers.iter().any(|e| e.name == LIVE_ENTRY);
    if added {
        servers.push(new_entry());
    }
    let entry = servers
        .iter_mut()
        .find(|e| e.name == LIVE_ENTRY)
        .expect("entry ensured above");
    let previous = entry
        .network
        .take()
        .map(|n| n.allow_hosts)
        .unwrap_or_default();
    // `network` is final here: merging (the default) already happened in
    // [`merge_hosts`], before the preview, so the preview shows what is written.
    entry.network = Some(network);
    Applied { added, previous }
}

/// The live entry's network policy: `Restricted` to exactly `hosts`.
pub(super) fn live_network(hosts: &[String]) -> McpServerNetwork {
    McpServerNetwork {
        mode: McpNetMode::Restricted,
        allow_hosts: hosts.to_vec(),
        ..Default::default()
    }
}

/// Default `/browser live` (#2101): keep the hosts already allowed, in their
/// order, and add the new ones after them. A rerun that only meant to attach `--profile`
/// used to silently drop every host `/browser allow` had added. Only a
/// `Restricted` policy is merged into a `Restricted` one; any other existing
/// mode has no host list worth keeping. Returns the kept hosts that this call
/// did not name, so the summary can say what `--replace` would have dropped.
pub(super) fn merge_hosts(
    after: &mut McpServerNetwork,
    existing: Option<&McpServerNetwork>,
) -> Vec<String> {
    let Some(prev) = existing.filter(|n| n.mode == McpNetMode::Restricted) else {
        return Vec::new();
    };
    if after.mode != McpNetMode::Restricted {
        return Vec::new();
    }
    let kept: Vec<String> = prev
        .allow_hosts
        .iter()
        .filter(|h| !after.allow_hosts.contains(h))
        .cloned()
        .collect();
    // Previous order first, so naming a host already allowed is a no-op.
    let mut merged = prev.allow_hosts.clone();
    for h in &after.allow_hosts {
        if !merged.contains(h) {
            merged.push(h.clone());
        }
    }
    after.allow_hosts = merged;
    kept
}

/// Gate on `mur browser setup`: refuse (and name the command) when not ready.
pub(super) fn check_setup(ready: bool) -> Result<(), String> {
    if ready {
        Ok(())
    } else {
        Err(
            "the browser is not installed — run `mur browser setup --yes` in a terminal \
             first (it needs npm and the registry, which an agent's seal does not grant)"
                .into(),
        )
    }
}

/// `--research`'s policy: all hosts, audited, with who enabled it and when.
pub(super) fn research_network(by: String, at_ms: u64) -> McpServerNetwork {
    crate::cmd::agent::mcp_network::broad_audited_network(vec![], by, at_ms)
}

/// `--research` reruns keep the deny list already on the entry: the flag
/// takes no hosts, so silently emptying a deny list would lift blocks the
/// user never asked to lift (#1919 review). A `Restricted` policy has no
/// deny list to carry.
pub(super) fn carry_deny_hosts(after: &mut McpServerNetwork, existing: Option<&McpServerNetwork>) {
    if after.mode == McpNetMode::BroadAudited
        && after.deny_hosts.is_empty()
        && let Some(prev) = existing.filter(|n| n.mode == McpNetMode::BroadAudited)
    {
        after.deny_hosts = prev.deny_hosts.clone();
    }
}

/// Same policy, ignoring who authorized it and when: a `--research` rerun
/// mints a fresh authorization but changes nothing the proxy enforces.
fn same_policy(a: &McpServerNetwork, b: &McpServerNetwork) -> bool {
    (a.mode, &a.allow_hosts, &a.deny_hosts) == (b.mode, &b.allow_hosts, &b.deny_hosts)
}

/// `--run` for the live entry: `live-<agent>`. Agent names are already
/// restricted to `[A-Za-z0-9_-]` (`validate_agent_name`), so no sanitizing;
/// per-agent so two agents never share a run even if runs are global.
pub(super) fn live_argv(agent: &str, profile: Option<&str>) -> Vec<String> {
    // Order matters: `--run` before `--mode`, as `cli/actions.rs` declares.
    let mut argv: Vec<String> = [
        "browser",
        "record",
        "--run",
        &format!("live-{agent}"),
        "--mode",
        "live",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // The saved session, injected as `--storage-state` by `cmd::browser::
    // live_state`. Absent = no cookies, which authenticated sites answer with
    // a redirect to their login page.
    if let Some(site) = profile {
        argv.push("--profile".to_owned());
        argv.push(site.to_owned());
    }
    argv
}

/// Filesystem grants the sealed live server cannot start without (#1639
/// layer 2: missing any one exits the server with `Operation not permitted`
/// before `tools/list`):
///
/// * read on the pinned MCP server install — `node` must open its script;
/// * write on the browser root — run dirs, profiles, the npm/pw scratch;
/// * write on Playwright's server registry — a `browser@<hash>` lock per
///   launch. Skipped when it cannot be located (no `HOME`); the probe then
///   reports the denial rather than this guessing a path.
pub(super) fn live_fs(mur_home: &Path, registry: Option<&Path>) -> FilesystemEntitlement {
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let mut write = vec![s(&mur_browser::paths::browser_root(mur_home))];
    write.extend(registry.map(s));
    FilesystemEntitlement {
        read: vec![s(&mur_browser::server::install_dir(mur_home))],
        write,
        deny: Vec::new(),
    }
}

/// The `mur` the entry should launch: this very binary, by absolute path.
/// A bare `mur` is resolved against whatever PATH the runtime has, so the
/// pinned hash and the exec'd file could drift apart (B0 rule 6). The
/// `murmur` alias is normalized, or `murmur browser …` would mis-dispatch.
pub(super) fn live_command(exe: std::io::Result<std::path::PathBuf>) -> String {
    match exe {
        Ok(p) => super::multiplex::canonical_mur_exe(p)
            .to_string_lossy()
            .into_owned(),
        Err(_) => "mur".into(),
    }
}

/// Point an existing live entry at `command` (from [`live_command`]) and
/// re-pin it to `sha`, the hash of that very file. An entry made before
/// absolute paths existed still says bare `mur`, which leaves the exec'd
/// binary up to the runtime's PATH (B0 rule 6). `description_hash` is kept:
/// it covers the Playwright server's tools, which `mur browser record`
/// launches unchanged whichever `mur` starts it. Returns a note on change.
pub(super) fn repin_command(
    entry: &mut McpServerEntry,
    command: &str,
    sha: String,
) -> Option<String> {
    let same_pin = entry
        .binary_sha256
        .as_deref()
        .is_some_and(|old| old.eq_ignore_ascii_case(&sha));
    if entry.command == command && same_pin {
        return None;
    }
    let note = format!(
        "re-pinned `{LIVE_ENTRY}` to {command} (sha256 {}…)",
        &sha[..16.min(sha.len())]
    );
    entry.command = command.to_string();
    entry.binary_sha256 = Some(sha);
    Some(note)
}

/// Add `command` to the spawn allowlist if it is not there verbatim. The
/// runtime resolves a bare name through its search dirs, so a re-pinned
/// absolute `command` needs its own literal entry or the seal denies the exec
/// (EPERM) even though `mur` is allowlisted. Existing entries are kept.
pub(super) fn ensure_spawn_allowed(allowed: &mut Vec<String>, command: &str) -> Option<String> {
    if allowed.iter().any(|a| a == command) {
        return None;
    }
    allowed.push(command.to_string());
    Some(format!("allowed spawn of {command}"))
}

/// The build dir of the newest complete headless shell under `browsers`
/// (#1639). The sealed live server execs that binary, which no allowlisted
/// program covers, so the grant is the build dir — the same lane `perm
/// allow-spawn-dir` opens, scoped to one Playwright revision.
pub(super) fn live_spawn_dir(browsers: Option<&Path>) -> Option<String> {
    let dir = browsers?;
    let exe = mur_browser::chromium::headless_shell_exe(dir)?;
    let build = exe.strip_prefix(dir).ok()?.components().next()?;
    Some(dir.join(build).to_string_lossy().into_owned())
}

/// Add `dir` to the spawn-dir allowlist if absent; one note when it changed.
pub(super) fn ensure_spawn_dir(dirs: &mut Vec<String>, dir: &str) -> Option<String> {
    if dirs.iter().any(|d| d == dir) {
        return None;
    }
    dirs.push(dir.to_string());
    Some(format!("allowed spawn under {dir}"))
}

/// Reply when a rerun changes nothing: no restart, nothing written.
pub(super) fn unchanged_text(hosts: &[String]) -> String {
    format!(
        "browser live mode already set up for: {} — nothing changed, no restart needed.\n\
         (If you skipped the restart after an earlier change, restart the agent to apply it.)",
        hosts.join(", ")
    )
}

/// Chip label for the restart live mode needs.
pub(super) const RESTART_LABEL: &str = "restart to enable browser live mode";

/// `/browser live` flags other than `--profile`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct LiveFlags {
    /// The user confirmed the preview: write it.
    pub yes: bool,
    /// Research mode: all hosts, audited (`BroadAudited`).
    pub research: bool,
    /// Set the allowlist to exactly the named hosts instead of adding them.
    pub replace: bool,
}

/// Strip `--yes` / `--research` / `--replace`, leaving the profile flags and the hosts.
pub(super) fn split_flags(args: &[String]) -> (Vec<String>, LiveFlags) {
    let mut flags = LiveFlags::default();
    let mut rest = Vec::with_capacity(args.len());
    for a in args {
        match a.as_str() {
            "--yes" => flags.yes = true,
            "--research" => flags.research = true,
            "--replace" => flags.replace = true,
            _ => rest.push(a.clone()),
        }
    }
    (rest, flags)
}

/// The slash command (no leading `/`) that confirms a preview. Hosts are the
/// normalized ones, so what the user approves is exactly what gets written.
pub(super) fn confirm_command(
    hosts_args: &[String],
    profile: &ProfileArg,
    flags: LiveFlags,
) -> String {
    let mut cmd = vec!["browser live --yes".to_owned()];
    if flags.research {
        cmd.push("--research".to_owned());
    }
    if flags.replace {
        cmd.push("--replace".to_owned());
    }
    cmd.extend(hosts_args.iter().cloned());
    match profile {
        ProfileArg::Set(site) => cmd.push(format!("--profile {site}")),
        ProfileArg::Clear => cmd.push("--no-profile".to_owned()),
        ProfileArg::Keep => {}
    }
    cmd.join(" ")
}

/// Chip label for the confirm step.
pub(super) const CONFIRM_LABEL: &str = "save this browser network policy";

/// The confirm chip, or `None` when the command would not pass
/// [`mur_common::proposal::vet`] (e.g. too long for a chip).
fn confirm_chip(cmd: &str) -> Option<Proposal> {
    let args = serde_json::json!({ "label": CONFIRM_LABEL, "kind": "slash", "command": cmd });
    mur_common::proposal::vet(&args).ok()
}

/// The whole `/browser live` flow. Every refusal returns before the first
/// write, so "refused" always means "nothing changed, no chip".
pub(super) fn run(
    agent: &str,
    args: &[String],
    ready: bool,
    ops: &mut impl LiveOps,
) -> anyhow::Result<Managed> {
    let (args, flags) = split_flags(args);
    let (host_args, profile) = split_profile(&args).map_err(anyhow::Error::msg)?;
    let (hosts, input_notes, after) = if flags.research {
        if !host_args.is_empty() {
            anyhow::bail!(
                "research mode takes no hosts — it allows every host and logs each connection; \
                 nothing was changed. Run `/browser live --research` alone, or name sites without it"
            );
        }
        if flags.replace {
            anyhow::bail!(
                "`--replace` scopes a host list and research mode has none; nothing was changed. \
                 Run `/browser live --research` alone"
            );
        }
        let by = std::env::var("USER").unwrap_or_else(|_| "unknown".to_owned());
        let at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        (Vec::new(), Vec::new(), research_network(by, at_ms))
    } else {
        let parsed = parse_hosts(&host_args).map_err(anyhow::Error::msg)?;
        let after = live_network(&parsed.hosts);
        (parsed.hosts, parsed.notes, after)
    };
    check_setup(ready).map_err(anyhow::Error::msg)?;
    let mut servers = ops.servers()?;
    // #1905 Part 2: show what will be written, and write only on `--yes`.
    let existing = servers.iter().find(|e| e.name == LIVE_ENTRY);
    let mut after = after;
    carry_deny_hosts(&mut after, existing.and_then(|e| e.network.as_ref()));
    let kept = if flags.replace {
        Vec::new()
    } else {
        merge_hosts(&mut after, existing.and_then(|e| e.network.as_ref()))
    };
    // What the entry may reach once written: every host, not just the new ones.
    let reach = after.allow_hosts.clone();
    let preview = render_preview(
        agent,
        LIVE_ENTRY,
        existing.and_then(|e| e.network.as_ref()),
        &after,
        &input_notes,
        DENY_OVERLAY_ENFORCEMENT,
    );
    if !flags.yes {
        // A rerun that changes nothing never offers a chip. With `--yes` the
        // write path below decides, so `save` can still backfill grants.
        let profile_same = match &profile {
            ProfileArg::Keep => true,
            p => existing.is_some_and(|e| e.args == live_argv(agent, p.site())),
        };
        let policy_same =
            existing.is_some_and(|e| e.network.as_ref().is_some_and(|n| same_policy(n, &after)));
        // An older entry can match the policy yet still need the re-pin or
        // grants `save` backfills; that is a change, so preview it (#1919).
        let repairs = if existing.is_some() {
            ops.pending_repairs(&servers)?
        } else {
            Vec::new()
        };
        if policy_same && profile_same && repairs.is_empty() {
            return Ok((unchanged_text(&reach), None));
        }
        let effect = Effect::predicted(
            existing.and_then(|e| e.network.as_ref()).map(|n| n.mode),
            after.mode,
            !repairs.is_empty(),
        );
        let preview = with_effect_line(&preview, effect);
        let preview = if repairs.is_empty() {
            preview
        } else {
            format!("{preview}\nalso repairs:\n  {}", repairs.join("\n  "))
        };
        let cmd = confirm_command(&hosts, &profile, flags);
        return Ok(match confirm_chip(&cmd) {
            Some(chip) => (preview, Some(chip)),
            None => (format!("{preview}\nRun: `/{cmd}`"), None),
        });
    }
    let mut notes = vec![preview];
    // `mcp_add` bails on a duplicate name by contract; decide here instead.
    let (previous, effect) = if servers.iter().any(|e| e.name == LIVE_ENTRY) {
        // Existing entry: write the (merged or replaced) policy. The save is atomic
        // (`save_profile` → `write_atomic`), so a failed save leaves the old,
        // still-valid policy in place — nothing to roll back.
        let before = servers.clone();
        let out = apply_network(&mut servers, after.clone(), || unreachable!("entry exists"));
        // A rerun that names a profile re-points the existing entry at it:
        // otherwise an entry created before `--profile` existed keeps
        // launching sessionless forever. `--no-profile` drops it.
        if profile != ProfileArg::Keep
            && let Some(entry) = servers.iter_mut().find(|e| e.name == LIVE_ENTRY)
        {
            entry.args = live_argv(agent, profile.site());
        }
        let entry_changed = servers != before;
        let (saved, effect) = ops
            .save(servers)
            .map_err(|e| e.context("could not save the browser live allowlist"))?;
        // `save` reports one note per change it made (re-pin, grant, spawn),
        // so no notes + same entry means the profile is exactly as it was:
        // a restart would reload identical config, so offer none.
        if !entry_changed && saved.is_empty() {
            return Ok((unchanged_text(&reach), None));
        }
        notes.extend(saved);
        (out.previous, effect)
    } else {
        // New entry: created WITH its policy in a single save. The probe sees
        // it `Restricted` (so it runs behind the egress proxy, #1639), and
        // there is no on-disk moment where `browser` exists unrestricted — so
        // no rollback path is needed.
        notes.push(ops.mcp_add(&live_argv(agent, profile.site()), after.clone())?);
        // D3: a new MCP server is a restart.
        (Vec::new(), Effect::Restart)
    };
    // The preview was rendered before the write; say what the write did.
    notes[0] = with_effect_line(&notes[0], effect);
    if flags.research {
        notes.push("browser live mode may reach: any host (research mode, audited)".to_owned());
        // Telemetry must not undo a write that already succeeded.
        if let Err(e) = ops.record_research(agent) {
            notes.push(format!(
                "could not record the research-mode audit event: {e:#}"
            ));
        }
    } else {
        notes.push(format!("browser live mode may reach: {}", reach.join(", ")));
    }
    match &profile {
        ProfileArg::Set(site) => notes.push(format!("signed in as the saved profile `{site}`")),
        ProfileArg::Clear => notes.push(
            "profile unbound: the browser now carries no session; the saved profile itself \
             is kept, so `--profile <site>` binds it again."
                .to_owned(),
        ),
        ProfileArg::Keep => notes.push(
            "no profile: the browser carries no session, so pages behind a login will \
             redirect there. Add one with `mur browser auth <site> --url <login url>`, \
             then rerun with `--profile <site>`."
                .to_owned(),
        ),
    }
    if !kept.is_empty() {
        notes.push(format!(
            "kept from the previous allowlist: {} — rerun with `--replace` to allow only the hosts you named",
            kept.join(", ")
        ));
    } else if flags.replace && !previous.is_empty() && previous != hosts {
        notes.push(format!(
            "replaced previous allowlist: {}",
            previous.join(", ")
        ));
    }
    notes.push(effect.applied_line().to_owned());
    let text = notes.join("\n");
    Ok(match effect {
        Effect::NextTurn => (text, None),
        Effect::Restart => (text, Some(Proposal::restart(RESTART_LABEL))),
    })
}

/// `preview` with its "takes effect" line replaced by `effect`'s. The preview
/// is shared with `/browser allow` and `mcp-network`, which keep theirs.
fn with_effect_line(preview: &str, effect: Effect) -> String {
    let cold = Effect::Restart.preview_line();
    if preview.contains(cold) {
        preview.replacen(cold, effect.preview_line(), 1)
    } else {
        preview.to_owned()
    }
}

/// `mur browser setup`: ensure the agent HAS the `browser` entry at all, so
/// the model can see the browser tools instead of being told the tool does not
/// exist. Visibility only — the entry is created `Restricted` with an EMPTY
/// allowlist, which reaches nothing but loopback, so naming the sites stays
/// `/browser live <host>...`'s job and setup never widens egress by itself.
/// `Ok(None)` means the entry was already there and nothing was written.
pub(crate) fn ensure_entry(agent: &str) -> anyhow::Result<Option<String>> {
    let mut ops = ProfileOps(agent);
    if ops.servers()?.iter().any(|e| e.name == LIVE_ENTRY) {
        return Ok(None);
    }
    ops.mcp_add(&live_argv(agent, None), live_network(&[]))
        .map(Some)
}

#[cfg(test)]
#[path = "browser_live_cmd_tests.rs"]
mod tests;
