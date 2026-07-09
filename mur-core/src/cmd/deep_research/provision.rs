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

/// Name of the gateway MCP server entry mounted on every worker.
const GATEWAY_MCP_NAME: &str = "research-gateway";

/// Binary invoked for the gateway MCP server (installed on PATH by
/// `build.sh`, shipped by Tasks 1-6).
const GATEWAY_MCP_COMMAND: &str = "mur-research-gateway";

/// Upper bound on `--count`: provisioning creates one agent dir + Ed25519
/// identity + runtime symlink per worker, so an unbounded count is a foot-gun.
/// Named to avoid an inline literal (mandatory rule 1).
const MAX_WORKER_COUNT: usize = 64;

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
        // Fix 1: bind model_ref to `model` (default DEFAULT_WORKER_MODEL) so
        // the worker resolves real credentials via the models.yaml registry
        // instead of falling to the ollama/llama3.2:3b StubEcho default.
        cmd_create(&name, true, None, Some(model.to_string()), None)?;
        cmd_mcp_add(
            &name,
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
        let (path, mut profile) = load_profile_for_edit(&name)?;
        profile.entitlements.network.outbound.allow_hosts = WORKER_LLM_ALLOW_HOSTS
            .iter()
            .map(|h| h.to_string())
            .collect();
        save_profile(&path, &mut profile)?;

        names.push(name);
    }
    Ok(names)
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
#[allow(clippy::too_many_arguments)]
pub fn cmd_provision(
    mur_home: &Path,
    name_prefix: Option<&str>,
    count: Option<usize>,
    model: Option<&str>,
    grant_egress_flag: bool,
    deny_hosts: &[String],
    yes: bool,
) -> Result<()> {
    let prefix = name_prefix.unwrap_or(DEFAULT_WORKER_PREFIX);
    let count = count.unwrap_or(DEFAULT_WORKER_COUNT);
    let model = model.unwrap_or(DEFAULT_WORKER_MODEL);
    let names = provision(mur_home, prefix, count, model)?;
    println!("Provisioned {} deep-research worker agent(s):", names.len());
    for name in &names {
        println!("  {name}");
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

        let names = provision(tmp.path(), "dr_worker", 3, DEFAULT_WORKER_MODEL).unwrap();
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

        // Fix 2: worker keeps `restricted` mode but the allow-list now
        // includes loopback so it can reach its own LLM endpoint.
        assert_eq!(
            p.entitlements.network.outbound.mode,
            mur_common::agent::NetworkOutboundMode::Restricted
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

    #[test]
    fn provision_threads_explicit_model_alias() {
        let _lock = MUR_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        unsafe {
            std::env::set_var("MUR_AGENT_BIN_DIR", &bin_dir);
        }
        seed_models_yaml(tmp.path(), "claude_sonnet", "anthropic", "claude-sonnet-5");

        let names = provision(tmp.path(), "dr_worker", 1, "claude_sonnet").unwrap();
        let p = mur_common::agent::AgentProfile::load(tmp.path(), &names[0]).unwrap();
        assert_eq!(p.model_ref, Some("claude_sonnet".to_string()));
    }

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

        let names = provision(tmp.path(), "dr_worker", 1, DEFAULT_WORKER_MODEL).unwrap();
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

        let zero = provision(tmp.path(), "dr_worker", 0, DEFAULT_WORKER_MODEL);
        assert!(zero.is_err(), "count==0 must error");

        let too_many = provision(
            tmp.path(),
            "dr_worker",
            MAX_WORKER_COUNT + 1,
            DEFAULT_WORKER_MODEL,
        );
        assert!(too_many.is_err(), "count > MAX_WORKER_COUNT must error");
    }
}
