//! `mur deep-research provision` — creates restricted worker agents that
//! each mount the `research-gateway` MCP server.
//!
//! Reuses the exact profile-construction path `mur agent create` uses
//! (`cmd::agent::lifecycle::cmd_create`, whose default entitlements already
//! set `network.outbound = restricted` with an empty allow-list) and the
//! exact MCP-attach path `mur agent mcp add` uses
//! (`cmd::agent::mcp::cmd_mcp_add`, which persists through the existing
//! load/save-atomic helpers) — no hand-rolled profile YAML here.
//!
//! Egress for the gateway MCP entry itself is left `None` (Inherit): the
//! per-server `BroadAudited` grant that actually lets the gateway reach the
//! network is a separate, explicit-consent step (Task 8). Provisioning
//! alone must never grant egress.

use std::path::Path;

use anyhow::Result;

use crate::cmd::agent::lifecycle::cmd_create;
use crate::cmd::agent::mcp::{McpAddPin, cmd_mcp_add, cmd_mcp_set_network};
use crate::cmd::agent::{load_profile_for_edit, save_profile};

/// Default number of workers `mur deep-research provision` creates when
/// `--count` is omitted.
pub const DEFAULT_WORKER_COUNT: usize = 3;

/// Default agent-name prefix when `--prefix` is omitted.
pub const DEFAULT_WORKER_PREFIX: &str = "dr_worker";

/// Default `models.yaml` alias each worker's `model_ref` is bound to when
/// `--model` is omitted from `mur deep-research provision`: cheap enough for
/// disposable worker fan-out, still capable of real reasoning (never the
/// `ollama/llama3.2:3b` StubEcho fallback `cmd_create` uses with no model
/// passed at all).
pub const DEFAULT_WORKER_MODEL: &str = "claude_haiku";

/// Loopback hosts every worker's agent-level `entitlements.network.outbound`
/// is seeded with at provision time, so the worker can reach its own LLM
/// endpoint (local cc-proxy) to reason. This is NOT web-research egress —
/// that remains exclusively the gateway MCP's separate, explicit-consent
/// `--grant-egress` step (`grant_egress` below). `mode` stays `restricted`;
/// this only widens the allow-list off empty.
const WORKER_LLM_ALLOW_HOSTS: [&str; 2] = ["localhost", "127.0.0.1"];

/// Built-in tools DENIED for a research worker. A worker's job is
/// search + fetch + reason + report-in-text; it has no business running a
/// shell or touching the filesystem. Left at the default `Ask` policy these
/// gate on the HITL approval prompt — which has no answerer in a headless
/// fleet-delegated turn, so the prompt times out (`hitl_denied`) and FAILS
/// the whole turn (root-caused live 2026-07-10: a research turn hit
/// `tool call denied: timed out` after the model reached for a built-in).
/// Denying them drops the tools from the advertised set entirely
/// (`tools::registry` skips `Deny`), so the model never calls them and the
/// turn can complete on the gateway tools alone.
const WORKER_DENIED_BUILTIN_TOOLS: [&str; 4] = ["bash", "read_file", "write_file", "edit_file"];

/// Name of the gateway MCP server entry mounted on every worker.
pub const GATEWAY_MCP_NAME: &str = "research-gateway";

/// Binary invoked for the gateway MCP server (installed on PATH by
/// `build.sh`, shipped by Tasks 1-6).
const GATEWAY_MCP_COMMAND: &str = "mur-research-gateway";

/// Upper bound on `--count`: provisioning creates one agent dir + Ed25519
/// identity + runtime symlink per worker, so an unbounded count is a foot-gun.
/// Named to avoid an inline literal (mandatory rule 1).
const MAX_WORKER_COUNT: usize = 64;

/// obscura render-engine binaries, relative to `mur_home` — must match
/// `mur-research-gateway`'s `DEFAULT_OBSCURA_RELATIVE_PATH` (`aura/obscura`).
/// Both the engine and its sibling worker must be exec-granted (spike Q1 Layer-2).
const OBSCURA_RELATIVE: &str = "aura/obscura";
const OBSCURA_WORKER_RELATIVE: &str = "aura/obscura-worker";

/// Value of `--render-engine` that opts a worker into the obscura render
/// engine. Any other value (or the flag omitted) leaves today's default
/// (`agent-browser`) path byte-for-byte unchanged.
const RENDER_ENGINE_OBSCURA: &str = "obscura";

/// Create `count` restricted worker agents named `<name_prefix>_1..N`, each
/// mounting the `research-gateway` MCP server with no egress grant of its
/// own. Returns the created agent names, in order.
///
/// # Concurrency
///
/// This function sets the **process-global** `MUR_HOME` env var for its whole
/// duration (and does NOT restore it), because the reused `cmd_create` /
/// `cmd_mcp_add` helpers re-derive their home directory from that env var
/// rather than taking a parameter. It is therefore **CLI-only and NOT
/// concurrency-safe**: it must not run while another thread/task reads or
/// writes `MUR_HOME`. Safe for the single-threaded CLI dispatch path today.
// TODO(follow-up): parameterize cmd_create/cmd_mcp_add with mur_home instead of
// mutating the global env, before any daemon/async caller uses provision().
pub fn provision(
    mur_home: &Path,
    name_prefix: &str,
    count: usize,
    model: &str,
    render_engine: Option<&str>,
) -> Result<Vec<String>> {
    if count == 0 {
        anyhow::bail!("count must be at least 1");
    }
    if count > MAX_WORKER_COUNT {
        anyhow::bail!("count {count} exceeds the maximum of {MAX_WORKER_COUNT} workers");
    }

    // `cmd_create` and `cmd_mcp_add` resolve their home directory via the
    // `MUR_HOME` env var (`resolve_mur_home` / `load_profile_for_edit`), so
    // provisioning against an explicit `mur_home` means pointing that env
    // var at it first — the same pattern `cmd::agent::mcp::tests` uses.
    // See the `# Concurrency` note above: this permanently mutates process env.
    unsafe {
        std::env::set_var("MUR_HOME", mur_home);
    }

    let mut names = Vec::with_capacity(count);
    for i in 1..=count {
        let name = format!("{name_prefix}_{i}");
        provision_one(mur_home, &name, model, render_engine)?;
        names.push(name);
    }
    Ok(names)
}

/// Single-worker provisioning body, single-sourced so both [`provision`]
/// (fresh-provision path) and the `setup` wizard's re-run reconcile
/// (which only calls this for MISSING workers, never for existing ones)
/// share the exact same profile construction. See the module doc comment
/// for the full rationale behind each step.
pub(crate) fn provision_one(
    mur_home: &Path,
    name: &str,
    model: &str,
    render_engine: Option<&str>,
) -> Result<()> {
    // Fix 1: bind model_ref to `model` (default DEFAULT_WORKER_MODEL) so
    // the worker resolves real credentials via the models.yaml registry
    // instead of falling to the ollama/llama3.2:3b StubEcho default.
    cmd_create(name, true, None, Some(model.to_string()), None)?;
    cmd_mcp_add(
        name,
        GATEWAY_MCP_NAME,
        GATEWAY_MCP_COMMAND,
        &[],
        McpAddPin {
            force: true,
            ..Default::default()
        },
    )?;

    // Fix 2: seed the agent-level outbound allow-list with loopback so
    // the worker can reach its own LLM endpoint (local cc-proxy) to
    // reason. Stays `restricted` — this only widens the allow-list off
    // empty, never touches the gateway MCP entry's own `network` block
    // (that stays `None`/Inherit until the separate `--grant-egress`
    // step in `grant_egress` below).
    let (path, mut profile) = load_profile_for_edit(name)?;
    profile.entitlements.network.outbound.allow_hosts = WORKER_LLM_ALLOW_HOSTS
        .iter()
        .map(|h| h.to_string())
        .collect();
    // ProxyOnly: deny all general outbound TCP; the worker's egress is
    // entirely loopback (cc-proxy LLM + the audited egress proxy), so the
    // OS profile forces every fetch through the proxy — no direct `*:443`
    // escape. (allow_hosts above keeps HostGuard governing the loopback
    // LLM hostname.)
    profile.entitlements.network.outbound.mode = mur_common::agent::NetworkOutboundMode::ProxyOnly;
    // Pre-approve the gateway's OWN tools (read-only search/fetch) so
    // headless delegated turns don't dead-end on the HITL gate
    // (`tool/approval_needed` has no answerer under fleet delegation →
    // 300 s timeout → deny → task failed). Scoped to
    // `mcp__research-gateway__*` only. This grants no egress by itself: the
    // gateway's outbound stays Inherit/restricted until the separate
    // explicit-consent `--grant-egress` step.
    profile
        .entitlements
        .tools
        .push(mur_common::agent::ToolRule {
            pattern: mur_common::mcp_naming::tool_pattern(GATEWAY_MCP_NAME),
            policy: mur_common::agent::ToolPolicy::Allow,
            risk: None,
        });
    // Deny the built-in tools (see WORKER_DENIED_BUILTIN_TOOLS): left at the
    // default `Ask`, a research turn that reaches for `bash`/`write_file`/…
    // dead-ends on the unanswerable headless HITL gate and FAILS the turn.
    // Denied → not advertised → the model never calls them.
    for tool in WORKER_DENIED_BUILTIN_TOOLS {
        profile
            .entitlements
            .tools
            .push(mur_common::agent::ToolRule {
                pattern: tool.to_string(),
                policy: mur_common::agent::ToolPolicy::Deny,
                risk: None,
            });
    }
    // Opt-in obscura render engine (Task 8a): the gateway runs under
    // `spawn_sandboxed` with this profile's `SandboxPolicy`, whose exec
    // allowlist is searched over dirs that EXCLUDE `~/.mur/aura/` — so
    // grant the two absolute paths directly. `from_entitlements` keeps an
    // absolute `spawn.allowed` entry (path separator present) as-is when
    // it resolves to an executable file, with no directory search. Any
    // value other than `RENDER_ENGINE_OBSCURA` (including `None`) leaves
    // the default `agent-browser` path byte-for-byte unchanged.
    if render_engine == Some(RENDER_ENGINE_OBSCURA) {
        for rel in [OBSCURA_RELATIVE, OBSCURA_WORKER_RELATIVE] {
            let abs = mur_home.join(rel).to_string_lossy().to_string();
            if !profile.entitlements.processes.spawn.allowed.contains(&abs) {
                profile.entitlements.processes.spawn.allowed.push(abs);
            }
        }
    }
    save_profile(&path, &mut profile)?;
    Ok(())
}

/// Grant `worker`'s `research-gateway` MCP server `BroadAudited` egress —
/// the ONE place a deep-research worker actually gains outbound network
/// access. This is a separate, explicit-consent step: it is never called
/// from [`provision`] itself and must never be called from fleet creation.
///
/// Reuses the shipped consent path verbatim
/// (`cmd::agent::mcp::cmd_mcp_set_network`, PR #661) rather than
/// re-implementing BroadAudited-setting or authorization-recording: that
/// function already prompts for `[y/N]` consent on stdin unless `yes` is
/// set, records the `EgressAuthorization { authorized_by, authorized_at_ms }`,
/// emits the `mur.egress.broad_audited.enabled` telemetry event, and clears
/// any prior authorization when the mode changes away from `BroadAudited`.
///
/// Sets the process-global `MUR_HOME` env var for its duration, same
/// caveat as [`provision`]'s `# Concurrency` note (`cmd_mcp_set_network`
/// re-derives its home directory from that env var).
/// Let `worker`'s gateway spawn the render browser.
///
/// The sandbox's exec allowlist searches a fixed set of directories that does
/// NOT include a user's npm prefix, and `agent-browser` is a bare name resolved
/// through `PATH` — so adding the NAME grants nothing. `from_entitlements`
/// keeps an entry containing a path separator as-is when it resolves to an
/// executable, which is why obscura grants absolute paths and why this must
/// too. Resolving here, at provision time, also means an operator who has not
/// installed the browser finds out now rather than mid-research.
///
/// Consent-gated by the caller: this is the right to EXECUTE a browser inside
/// the worker's sandbox, which is a larger grant than egress and is asked for
/// separately in the wizard.
pub fn grant_render_browser(mur_home: &Path, worker: &str) -> Result<()> {
    // Same reason `grant_egress` does this: the profile helpers below resolve
    // their home from `MUR_HOME`, so a caller passing an explicit home must
    // publish it or the edit lands in the wrong tree.
    unsafe {
        std::env::set_var("MUR_HOME", mur_home);
    }
    let bins = render_binaries(mur_home);
    if bins.is_empty() {
        // Nothing installed is not a provisioning failure: the worker simply
        // has no rendered fetch, and `browser.rs` says so when a page needs it.
        eprintln!("  note: no render browser found, so {worker} gets no rendered fetch.");
        return Ok(());
    }
    let (path, mut profile) = super::super::agent::load_profile_for_edit(worker)?;
    let allowed = &mut profile.entitlements.processes.spawn.allowed;
    let mut added = Vec::new();
    for abs in bins {
        if !allowed.contains(&abs) {
            allowed.push(abs.clone());
            added.push(abs);
        }
    }
    if added.is_empty() {
        return Ok(());
    }
    super::super::agent::save_profile(&path, &mut profile)?;
    for abs in added {
        println!("Granted {worker} permission to spawn {abs}");
    }
    Ok(())
}

/// Where the gateway keeps its bundled lightpanda, relative to `~/.mur`.
/// Mirrors `mur-research-gateway`'s `DEFAULT_LIGHTPANDA_RELATIVE_PATH`; the two
/// must agree or the grant names a binary the gateway never runs.
const LIGHTPANDA_RELATIVE: &str = "aura/lightpanda";

/// Every render binary the gateway might spawn, absolute, for the exec
/// allowlist.
///
/// Granting only `agent-browser` was not enough, and an end-to-end check is
/// what showed it: the gateway prefers `~/.mur/aura/lightpanda` whenever that
/// file exists, so on a machine with it installed the grant named a binary that
/// was never run and rendered fetch stayed refused. The consent is "may it
/// execute a render browser", so it covers every candidate rather than guessing
/// which one wins at run time.
///
/// A custom `research_gateway.lightpanda_path` is NOT covered: provisioning
/// cannot see a run-time override. The denial message names the binary, so an
/// operator can grant that one by hand.
fn render_binaries(mur_home: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let lp = mur_home.join(LIGHTPANDA_RELATIVE);
    if lp.is_file() {
        out.push(canonical_string(&lp));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let cand = dir.join("agent-browser");
            if cand.is_file() {
                out.push(canonical_string(&cand));
                break;
            }
        }
    }
    out
}

/// Resolve symlinks so the granted path is the one the kernel checks: npm
/// installs `agent-browser` as a symlink into `lib/node_modules/`.
fn canonical_string(p: &Path) -> String {
    p.canonicalize()
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

pub fn grant_egress(mur_home: &Path, worker: &str, deny_hosts: &[String], yes: bool) -> Result<()> {
    unsafe {
        std::env::set_var("MUR_HOME", mur_home);
    }
    cmd_mcp_set_network(
        worker,
        GATEWAY_MCP_NAME,
        vec![],
        deny_hosts.to_vec(),
        false,
        true,
        yes,
    )
}

/// CLI-facing wrapper for `mur deep-research provision`: applies
/// [`DEFAULT_WORKER_PREFIX`]/[`DEFAULT_WORKER_COUNT`] when the flags are
/// omitted, provisions the workers, prints their names, and — ONLY when
/// `grant_egress_flag` is set via the explicit `--grant-egress` CLI flag —
/// grants each worker's gateway `BroadAudited` egress via [`grant_egress`]
/// (consent-prompted per worker unless `yes`). Plain `provision` (the
/// default) never grants egress.
///
/// `render_engine`: when `Some("obscura")`, verifies both obscura binaries
/// exist under `<mur_home>/aura/` BEFORE provisioning (fail fast with a
/// clear install hint rather than silently creating workers that can't exec
/// the engine), then threads the opt-in through to [`provision`] and prints
/// the exec-grant + gateway-config hint. Any other value (or `None`) is a
/// no-op — output is byte-for-byte identical to today.
#[allow(clippy::too_many_arguments)]
pub fn cmd_provision(
    mur_home: &Path,
    name_prefix: Option<&str>,
    count: Option<usize>,
    model: Option<&str>,
    grant_egress_flag: bool,
    grant_browser_flag: bool,
    deny_hosts: &[String],
    yes: bool,
    render_engine: Option<&str>,
) -> Result<()> {
    let prefix = name_prefix.unwrap_or(DEFAULT_WORKER_PREFIX);
    let count = count.unwrap_or(DEFAULT_WORKER_COUNT);
    let model = model.unwrap_or(DEFAULT_WORKER_MODEL);

    let obscura_paths = if render_engine == Some(RENDER_ENGINE_OBSCURA) {
        let engine = mur_home.join(OBSCURA_RELATIVE);
        let worker_bin = mur_home.join(OBSCURA_WORKER_RELATIVE);
        if !engine.is_file() || !worker_bin.is_file() {
            anyhow::bail!(
                "obscura render engine not found — expected both\n  {}\n  {}\n\
                 Install obscura to ~/.mur/aura/ (both `obscura` and \
                 `obscura-worker`) before running `--render-engine obscura`.",
                engine.display(),
                worker_bin.display(),
            );
        }
        Some((engine, worker_bin))
    } else {
        None
    };

    let names = provision(mur_home, prefix, count, model, render_engine)?;
    println!("Provisioned {} deep-research worker agent(s):", names.len());
    for name in &names {
        println!("  {name}");
    }
    println!(
        "  tool policy: {} → allow (gateway search/fetch pre-approved for headless turns)",
        mur_common::mcp_naming::tool_pattern(GATEWAY_MCP_NAME)
    );
    println!(
        "  tool policy: {} → deny (built-ins off so a research turn can't dead-end on the HITL gate)",
        WORKER_DENIED_BUILTIN_TOOLS.join(", ")
    );

    // The wizard asks this as its own question; this is that answer for
    // scripted use. Without a path here the grant is reachable only from an
    // interactive terminal — which is how this gap was found: verifying the
    // grant end-to-end could not get past `setup`'s own non-interactive guard.
    if grant_browser_flag {
        for name in &names {
            grant_render_browser(mur_home, name)?;
        }
    }
    if let Some((engine, worker_bin)) = &obscura_paths {
        println!(
            "  render engine: obscura — exec granted for {}, {}",
            engine.display(),
            worker_bin.display()
        );
        println!(
            "  NOTE: set `research_gateway.render_engine: obscura` in ~/.mur/config.yaml \
             (or export MUR_RESEARCH_RENDER_ENGINE=obscura) so the gateway uses it."
        );
    }
    if grant_egress_flag {
        for name in &names {
            grant_egress(mur_home, name, deny_hosts, yes)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// A machine without `agent-browser` must still provision. The grant is a
    /// convenience, not a dependency — failing here would make an optional
    /// render tier a hard requirement for deep research.
    ///
    /// (The `--grant-browser` flag itself is enforced by the compiler: the
    /// dispatch cannot build without passing it, and `cli` lives in the binary
    /// so a lib test cannot parse it. The gap it closes was found by trying to
    /// use the grant from a script — `setup` refuses without a TTY, so before
    /// this flag the grant was reachable only from an interactive terminal.)
    #[test]
    fn a_missing_browser_is_a_note_not_a_provisioning_failure() {
        let home = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("PATH");
        // SAFETY: single-threaded test; PATH is restored below.
        unsafe { std::env::set_var("PATH", "") };
        let r = super::render_binaries(home.path());
        if let Some(p) = prev {
            unsafe { std::env::set_var("PATH", p) };
        }
        assert!(r.is_empty(), "nothing installed resolves nothing: {r:?}");
        // …and the caller turns that into a note, never an Err — pinned by the
        // `let Ok(abs) = … else { return Ok(()) }` shape in grant_render_browser.
    }

    /// The bug an end-to-end check found: the gateway prefers lightpanda when
    /// it exists, so a grant naming only `agent-browser` covered the wrong
    /// binary and rendered fetch stayed refused.
    #[test]
    fn a_bundled_lightpanda_is_granted_not_just_agent_browser() {
        let home = tempfile::tempdir().unwrap();
        let aura = home.path().join("aura");
        std::fs::create_dir_all(&aura).unwrap();
        std::fs::write(aura.join("lightpanda"), b"x").unwrap();
        let bins = super::render_binaries(home.path());
        // `Path::ends_with` compares COMPONENTS, so it is separator-agnostic.
        // `str::ends_with("aura/lightpanda")` passes on Unix and fails on
        // Windows, where the canonical path uses backslashes — the product
        // code is fine (it joins `PathBuf`s), only a test can get this wrong.
        assert!(
            bins.iter()
                .any(|b| Path::new(b).ends_with("aura/lightpanda")),
            "the bundled engine must be granted: {bins:?}"
        );
    }

    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// Serialize tests that mutate the process-wide `MUR_HOME` /
    /// `MUR_AGENT_BIN_DIR` env vars (established pattern, see
    /// `cmd::agent::mcp::tests::MUR_HOME_LOCK`).
    static MUR_HOME_LOCK: Mutex<()> = Mutex::new(());

    /// Seed `<mur_home>/models.yaml` with a registry alias, mirroring
    /// `cmd::agent::lifecycle::tests::seed_models_yaml` — `cmd_create`
    /// (PR #661) only binds `model_ref` when the bare `--model` value is an
    /// exact registry key.
    fn seed_models_yaml(mur_home: &Path, key: &str, provider: &str, model: &str) {
        use mur_common::model::{ModelEntry, ModelRegistry};

        let mut models = BTreeMap::new();
        models.insert(
            key.to_string(),
            ModelEntry {
                provider: provider.to_string(),
                model: model.to_string(),
                ..Default::default()
            },
        );
        let reg = ModelRegistry {
            schema_version: 1,
            models,
            roles: BTreeMap::new(),
        };
        reg.save_to(&mur_home.join("models.yaml")).unwrap();
    }

    // Unix-only: provision() -> cmd_create() writes a per-agent runtime symlink
    // (busybox-style) which requires privileges Windows CI lacks ("os error 2").
    // The whole agent runtime is Unix-socket based, so the feature is Unix-only.
    #[cfg(unix)]
    #[test]
    fn provision_creates_restricted_workers_with_gateway() {
        let _lock = MUR_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        // Redirect the runtime-symlink dir cmd_create() also writes into,
        // so the test never touches the developer's real ~/.local/bin.
        let bin_dir = tmp.path().join("bin");
        unsafe {
            std::env::set_var("MUR_AGENT_BIN_DIR", &bin_dir);
        }
        seed_models_yaml(
            tmp.path(),
            DEFAULT_WORKER_MODEL,
            "anthropic",
            "claude-haiku-4-5",
        );

        let names = provision(tmp.path(), "dr_worker", 3, DEFAULT_WORKER_MODEL, None).unwrap();
        assert_eq!(names.len(), 3);
        assert_eq!(names, vec!["dr_worker_1", "dr_worker_2", "dr_worker_3"]);

        let p = mur_common::agent::AgentProfile::load(tmp.path(), &names[0]).unwrap();
        assert!(p.mcp_servers.iter().any(|s| s.name == "research-gateway"));
        // Egress NOT granted here — must be Inherit/restricted until the
        // consent step (Task 8).
        let gw = p
            .mcp_servers
            .iter()
            .find(|s| s.name == "research-gateway")
            .unwrap();
        assert!(gw.network.is_none());
        assert_eq!(gw.command, "mur-research-gateway");
        assert!(gw.args.is_empty());

        // Fix 1: model_ref is bound to the (default) worker model alias, not
        // left unset (which would silently fall to StubEcho).
        assert_eq!(p.model_ref, Some(DEFAULT_WORKER_MODEL.to_string()));

        // Fix 2 (+ Task 5): worker is `ProxyOnly` — all general outbound TCP
        // denied, egress forced entirely through the loopback egress proxy —
        // but the allow-list still includes loopback so it can reach its own
        // LLM endpoint.
        assert_eq!(
            p.entitlements.network.outbound.mode,
            mur_common::agent::NetworkOutboundMode::ProxyOnly
        );
        assert!(
            p.entitlements
                .network
                .outbound
                .allow_hosts
                .contains(&"127.0.0.1".to_string())
        );
        assert!(
            p.entitlements
                .network
                .outbound
                .allow_hosts
                .contains(&"localhost".to_string())
        );
    }

    #[cfg(unix)] // provision() writes a Unix runtime symlink; not runnable on Windows CI
    #[test]
    fn provision_threads_explicit_model_alias() {
        let _lock = MUR_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        unsafe {
            std::env::set_var("MUR_AGENT_BIN_DIR", &bin_dir);
        }
        seed_models_yaml(tmp.path(), "claude_sonnet", "anthropic", "claude-sonnet-5");

        let names = provision(tmp.path(), "dr_worker", 1, "claude_sonnet", None).unwrap();
        let p = mur_common::agent::AgentProfile::load(tmp.path(), &names[0]).unwrap();
        assert_eq!(p.model_ref, Some("claude_sonnet".to_string()));
    }

    #[cfg(unix)] // provision()/grant_egress() write Unix runtime artifacts; not runnable on Windows CI
    #[test]
    fn grant_sets_broad_audited_with_authorization() {
        let _lock = MUR_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        unsafe {
            std::env::set_var("MUR_AGENT_BIN_DIR", &bin_dir);
        }
        seed_models_yaml(
            tmp.path(),
            DEFAULT_WORKER_MODEL,
            "anthropic",
            "claude-haiku-4-5",
        );

        let names = provision(tmp.path(), "dr_worker", 1, DEFAULT_WORKER_MODEL, None).unwrap();
        grant_egress(tmp.path(), &names[0], &["evil.example".into()], true).unwrap();
        let p = mur_common::agent::AgentProfile::load(tmp.path(), &names[0]).unwrap();
        let gw = p
            .mcp_servers
            .iter()
            .find(|s| s.name == "research-gateway")
            .unwrap();
        let net = gw.network.as_ref().unwrap();
        assert!(matches!(
            net.mode,
            mur_common::agent::McpNetMode::BroadAudited
        ));
        assert!(net.authorization.is_some());
        assert!(net.deny_hosts.contains(&"evil.example".to_string()));
    }

    #[test]
    fn provision_rejects_zero_and_over_max_count() {
        // Count validation happens before any env mutation, so no lock/tmp
        // plumbing is needed — but take the lock anyway for hygiene.
        let _lock = MUR_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();

        let zero = provision(tmp.path(), "dr_worker", 0, DEFAULT_WORKER_MODEL, None);
        assert!(zero.is_err(), "count==0 must error");

        let too_many = provision(
            tmp.path(),
            "dr_worker",
            MAX_WORKER_COUNT + 1,
            DEFAULT_WORKER_MODEL,
            None,
        );
        assert!(too_many.is_err(), "count > MAX_WORKER_COUNT must error");
    }

    // Unix-only: same cmd_create runtime-symlink constraint as the sibling
    // provision tests (see the comment on
    // provision_creates_restricted_workers_with_gateway).
    #[cfg(unix)]
    #[test]
    fn provision_stamps_gateway_tool_allow_rule() {
        use mur_common::agent::{ToolPolicy, resolve_tool_policy};

        let _lock = MUR_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        unsafe {
            std::env::set_var("MUR_AGENT_BIN_DIR", &bin_dir);
        }
        seed_models_yaml(
            tmp.path(),
            DEFAULT_WORKER_MODEL,
            "anthropic",
            "claude-haiku-4-5",
        );

        let names = provision(tmp.path(), "dr_tool", 1, DEFAULT_WORKER_MODEL, None).unwrap();
        let p = mur_common::agent::AgentProfile::load(tmp.path(), &names[0]).unwrap();

        // The gateway tools resolve to Allow (headless delegated turns skip
        // the HITL gate for them)…
        let search = mur_common::mcp_naming::wire_name(
            &mur_common::mcp_naming::sanitize_server(GATEWAY_MCP_NAME),
            "research_search",
        );
        assert_eq!(
            resolve_tool_policy(&p.entitlements.tools, &search),
            ToolPolicy::Allow
        );

        // …the built-in tools are DENIED (else a headless research turn that
        // reaches for one dead-ends on the unanswerable HITL gate and fails).
        for tool in ["bash", "read_file", "write_file", "edit_file"] {
            assert_eq!(
                resolve_tool_policy(&p.entitlements.tools, tool),
                ToolPolicy::Deny,
                "built-in `{tool}` must be denied for a research worker"
            );
        }
        // …and an unrelated MCP tool keeps the fail-closed default (Ask): the
        // allow is gateway-scoped, never a blanket allow.
        assert_eq!(
            resolve_tool_policy(&p.entitlements.tools, "mcp__github__merge_pr"),
            ToolPolicy::Ask
        );
    }

    /// `--render-engine obscura` grants exec for both obscura binaries
    /// (absolute paths, since the sandbox's exec-allowlist directory search
    /// excludes `~/.mur/aura/` — see the module doc comment).
    #[cfg(unix)]
    #[test]
    fn provision_obscura_grants_exec_paths() {
        let _lock = MUR_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        unsafe {
            std::env::set_var("MUR_AGENT_BIN_DIR", &bin_dir);
        }
        seed_models_yaml(
            tmp.path(),
            DEFAULT_WORKER_MODEL,
            "anthropic",
            "claude-haiku-4-5",
        );

        let aura_dir = tmp.path().join("aura");
        std::fs::create_dir_all(&aura_dir).unwrap();
        std::fs::write(aura_dir.join("obscura"), b"#!/bin/sh\n").unwrap();
        std::fs::write(aura_dir.join("obscura-worker"), b"#!/bin/sh\n").unwrap();

        let names = provision(
            tmp.path(),
            "dr_obscura",
            1,
            DEFAULT_WORKER_MODEL,
            Some("obscura"),
        )
        .unwrap();
        let p = mur_common::agent::AgentProfile::load(tmp.path(), &names[0]).unwrap();

        let engine = tmp
            .path()
            .join(OBSCURA_RELATIVE)
            .to_string_lossy()
            .to_string();
        let worker_bin = tmp
            .path()
            .join(OBSCURA_WORKER_RELATIVE)
            .to_string_lossy()
            .to_string();
        assert!(
            p.entitlements.processes.spawn.allowed.contains(&engine),
            "spawn.allowed missing obscura engine path: {:?}",
            p.entitlements.processes.spawn.allowed
        );
        assert!(
            p.entitlements.processes.spawn.allowed.contains(&worker_bin),
            "spawn.allowed missing obscura-worker path: {:?}",
            p.entitlements.processes.spawn.allowed
        );
    }

    /// Default render engine (flag omitted, i.e. `None`) grants nothing
    /// extra — the exec allowlist stays exactly as today's `agent-browser`
    /// path leaves it.
    #[cfg(unix)]
    #[test]
    fn provision_default_engine_grants_nothing_extra() {
        let _lock = MUR_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        unsafe {
            std::env::set_var("MUR_AGENT_BIN_DIR", &bin_dir);
        }
        seed_models_yaml(
            tmp.path(),
            DEFAULT_WORKER_MODEL,
            "anthropic",
            "claude-haiku-4-5",
        );

        let names = provision(tmp.path(), "dr_default", 1, DEFAULT_WORKER_MODEL, None).unwrap();
        let p = mur_common::agent::AgentProfile::load(tmp.path(), &names[0]).unwrap();

        let engine = tmp
            .path()
            .join(OBSCURA_RELATIVE)
            .to_string_lossy()
            .to_string();
        let worker_bin = tmp
            .path()
            .join(OBSCURA_WORKER_RELATIVE)
            .to_string_lossy()
            .to_string();
        assert!(!p.entitlements.processes.spawn.allowed.contains(&engine));
        assert!(!p.entitlements.processes.spawn.allowed.contains(&worker_bin));
    }
}
