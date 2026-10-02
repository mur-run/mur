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
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("PATH", "");
    let r = super::render_binaries(home.path());
    if let Some(p) = prev {
        envg.set_var("PATH", p);
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

/// Seed `<mur_home>/models.yaml` with a registry alias, mirroring
/// `cmd::agent::lifecycle::tests::seed_models_yaml` — `cmd_create`
/// (PR #661) only binds `model_ref` when the bare `--model` value is an
/// exact registry key.
fn seed_models_yaml(mur_home: &Path, key: &str, provider: &str, model: &str) {
    // `provision`/`grant_*` set MUR_HOME for the helpers they call and
    // never put it back — their own `# Concurrency` note says so. Without
    // this the value outlives the test and every later one in the process
    // inherits it; with `--test-threads=1` it still does, because the leak
    // is not a race.
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.track_var("MUR_HOME");
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
    let mut envg = mur_common::test_env::EnvGuard::hold();
    // `provision`/`grant_*` set MUR_HOME for the helpers they call and
    // never put it back — their own `# Concurrency` note says so. Without
    // this the value outlives the test and every later one in the process
    // inherits it; with `--test-threads=1` it still does, because the leak
    // is not a race.
    envg.track_var("MUR_HOME");
    let tmp = tempfile::tempdir().unwrap();
    // Redirect the runtime-symlink dir cmd_create() also writes into,
    // so the test never touches the developer's real ~/.local/bin.
    let bin_dir = tmp.path().join("bin");
    envg.set_var("MUR_AGENT_BIN_DIR", &bin_dir);
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
    let mut envg = mur_common::test_env::EnvGuard::hold();
    // `provision`/`grant_*` set MUR_HOME for the helpers they call and
    // never put it back — their own `# Concurrency` note says so. Without
    // this the value outlives the test and every later one in the process
    // inherits it; with `--test-threads=1` it still does, because the leak
    // is not a race.
    envg.track_var("MUR_HOME");
    let tmp = tempfile::tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");
    envg.set_var("MUR_AGENT_BIN_DIR", &bin_dir);
    seed_models_yaml(tmp.path(), "claude_sonnet", "anthropic", "claude-sonnet-5");

    let names = provision(tmp.path(), "dr_worker", 1, "claude_sonnet", None).unwrap();
    let p = mur_common::agent::AgentProfile::load(tmp.path(), &names[0]).unwrap();
    assert_eq!(p.model_ref, Some("claude_sonnet".to_string()));
}

#[cfg(unix)] // provision()/grant_egress() write Unix runtime artifacts; not runnable on Windows CI
#[test]
fn grant_sets_broad_audited_with_authorization() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    // `provision`/`grant_*` set MUR_HOME for the helpers they call and
    // never put it back — their own `# Concurrency` note says so. Without
    // this the value outlives the test and every later one in the process
    // inherits it; with `--test-threads=1` it still does, because the leak
    // is not a race.
    envg.track_var("MUR_HOME");
    let tmp = tempfile::tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");
    envg.set_var("MUR_AGENT_BIN_DIR", &bin_dir);
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
    assert!(
        net.deny_hosts
            .contains(&LIGHTPANDA_TELEMETRY_HOST.to_string()),
        "every grant denies Lightpanda telemetry: {:?}",
        net.deny_hosts
    );
}

#[test]
fn egress_deny_list_always_adds_lightpanda_telemetry() {
    // setup passes an empty list — that is the case §7.1 was about.
    assert_eq!(egress_deny_list(&[]), vec![LIGHTPANDA_TELEMETRY_HOST]);
    assert_eq!(
        egress_deny_list(&["evil.example".into()]),
        vec!["evil.example", LIGHTPANDA_TELEMETRY_HOST]
    );
    // A user who already passed it (any case) does not get it twice.
    assert_eq!(
        egress_deny_list(&["Telemetry.Lightpanda.io".into()]),
        vec!["Telemetry.Lightpanda.io"]
    );
}

#[test]
fn provision_rejects_zero_and_over_max_count() {
    // Count validation happens before any env mutation, so no lock/tmp
    // plumbing is needed — but take the lock anyway for hygiene.
    // A reader: it needs the environment to hold still, not to change it.
    let mut _envg = mur_common::test_env::EnvGuard::hold();
    // `provision`/`grant_*` set MUR_HOME for the helpers they call and
    // never put it back — their own `# Concurrency` note says so. Without
    // this the value outlives the test and every later one in the process
    // inherits it; with `--test-threads=1` it still does, because the leak
    // is not a race.
    _envg.track_var("MUR_HOME");
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

    let mut envg = mur_common::test_env::EnvGuard::hold();
    // `provision`/`grant_*` set MUR_HOME for the helpers they call and
    // never put it back — their own `# Concurrency` note says so. Without
    // this the value outlives the test and every later one in the process
    // inherits it; with `--test-threads=1` it still does, because the leak
    // is not a race.
    envg.track_var("MUR_HOME");
    let tmp = tempfile::tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");
    envg.set_var("MUR_AGENT_BIN_DIR", &bin_dir);
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
    // Driven off the constant on purpose: a tool added to the deny list
    // (e.g. `open_item`, added after a live run failed with
    // `hitl_denied`) is covered here the moment it is added.
    for tool in WORKER_DENIED_BUILTIN_TOOLS {
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

/// Re-seeding a profile must not grow its tool-rule list.
///
/// Regression, root-caused live 2026-09-20: the gateway/deny seeding in
/// `provision_one` `push`ed unconditionally while the sibling
/// `spawn.allowed` seeding a few lines below had a `contains` guard. Any
/// second pass over an already-seeded profile appended another identical
/// copy of all five rules — `dr_worker_1`'s `profile.yaml` carried FIVE
/// `bash: deny` blocks (archive v106→v110), while its never-re-seeded
/// siblings `dr_worker_2`/`dr_worker_3` stayed at one apiece.
///
/// Driven through the helper rather than `provision` because `cmd_create`
/// refuses an existing agent ("agent {name} already exists at …"), so the
/// second pass can never come from a plain re-`provision`.
#[test]
fn reseeding_tool_rules_upserts_instead_of_appending() {
    use mur_common::agent::{ToolPolicy, ToolRule, resolve_tool_policy};

    let mut rules: Vec<ToolRule> = Vec::new();
    let gateway = mur_common::mcp_naming::tool_pattern(GATEWAY_MCP_NAME);

    // Two identical seeding passes, exactly as `provision_one` runs them.
    for _ in 0..2 {
        upsert_tool_rule(&mut rules, gateway.clone(), ToolPolicy::Allow);
        for tool in WORKER_DENIED_BUILTIN_TOOLS {
            upsert_tool_rule(&mut rules, tool.to_string(), ToolPolicy::Deny);
        }
    }

    assert_eq!(
        rules.len(),
        1 + WORKER_DENIED_BUILTIN_TOOLS.len(),
        "second seeding pass must upsert, not append: {rules:#?}"
    );
    for tool in WORKER_DENIED_BUILTIN_TOOLS {
        assert_eq!(
            rules.iter().filter(|r| r.pattern == tool).count(),
            1,
            "`{tool}` must appear exactly once after re-seeding"
        );
        assert_eq!(resolve_tool_policy(&rules, tool), ToolPolicy::Deny);
    }

    // An unrelated rule the user added by hand survives re-seeding, and a
    // pattern already present is UPDATED in place rather than shadowed.
    upsert_tool_rule(&mut rules, "open_item".to_string(), ToolPolicy::Allow);
    upsert_tool_rule(&mut rules, "bash".to_string(), ToolPolicy::Allow);
    assert_eq!(rules.iter().filter(|r| r.pattern == "bash").count(), 1);
    assert_eq!(resolve_tool_policy(&rules, "bash"), ToolPolicy::Allow);
    assert_eq!(resolve_tool_policy(&rules, "open_item"), ToolPolicy::Allow);
}

/// `--render-engine obscura` grants exec for both obscura binaries
/// (absolute paths, since the sandbox's exec-allowlist directory search
/// excludes `~/.mur/aura/` — see the module doc comment).
#[cfg(unix)]
#[test]
fn provision_obscura_grants_exec_paths() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    // `provision`/`grant_*` set MUR_HOME for the helpers they call and
    // never put it back — their own `# Concurrency` note says so. Without
    // this the value outlives the test and every later one in the process
    // inherits it; with `--test-threads=1` it still does, because the leak
    // is not a race.
    envg.track_var("MUR_HOME");
    let tmp = tempfile::tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");
    envg.set_var("MUR_AGENT_BIN_DIR", &bin_dir);
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
    let mut envg = mur_common::test_env::EnvGuard::hold();
    // `provision`/`grant_*` set MUR_HOME for the helpers they call and
    // never put it back — their own `# Concurrency` note says so. Without
    // this the value outlives the test and every later one in the process
    // inherits it; with `--test-threads=1` it still does, because the leak
    // is not a race.
    envg.track_var("MUR_HOME");
    let tmp = tempfile::tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");
    envg.set_var("MUR_AGENT_BIN_DIR", &bin_dir);
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
