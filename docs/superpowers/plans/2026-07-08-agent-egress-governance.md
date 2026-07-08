# Agent Egress Governance (Phase 1) + CLI Hardening — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the `broad-audited` network egress mode (deny-overlay + explicit consent + on-profile authorization record + telemetry) as the first rung of MUR's egress-governance ladder, plus fix 8 CLI bugs + the install-service PATH gap surfaced by the AURA build.

**Architecture:** Part A extends the existing entitlements data model (`OutboundNetwork` / `NetworkOutboundMode` in `mur-common`), the runtime sbpl enforcer (`mur-agent-runtime`), and the `mur agent perm` CLI (`mur-core`). The Phase-1 authorization record lives on the **agent profile** (not the fleet-loop `GovernanceState`, which is ephemeral) so it persists, travels with export, and is re-approved on import. Part B is independent mechanical fixes.

**Tech Stack:** Rust (edition 2024), serde, clap, macOS sbpl sandbox. Tests: `cargo test -p <crate> <name>` (nextest or plain; per repo memory, prefer `cargo test -p mur-common <name>` for unit scope, `ORT_STRATEGY=download` if mur-core lib is pulled).

## Global Constraints

- Do NOT change `restricted` / `unrestricted` / `off` semantics — additive only. — spec §6.
- New mode is named exactly `broad-audited` (serde `rename_all = "lowercase"` → the serialized token is `broad-audited`; use `#[serde(rename = "broad-audited")]` on the variant). — spec §2.2.
- `broad-audited` = allow all outbound MINUS `deny_hosts`; fail-closed on a denied host. — spec §2.6.
- Enabling `broad-audited` requires explicit operator consent and records `authorized_by` + `authorized_at_ms` on the profile; emits one telemetry event. — spec §2.6.
- Policy schema carries the `(agent, tool)` scoping binding from Phase 1 even though enforcement is Phase 4 — add the field, don't enforce it yet. — spec §2.3.
- Import never auto-grants `broad-audited`+ — downgrade to `restricted` on import. — spec §2.3.
- Phases 2–4 are roadmap only (§ Roadmap), not tasks. — spec §2.5.
- No hardcoded values (PATH derivation in G2 uses `npm config get prefix`, not a literal). — CLAUDE.md rule 1.

---

## File map

| File | Change |
|------|--------|
| `mur-common/src/agent.rs` | Add `BroadAudited` variant; add `deny_hosts`, `tool_scope`, `authorization` to `OutboundNetwork`; `EgressAuthorization` struct |
| `mur-agent-runtime/src/entitlements.rs` | Map `BroadAudited` → allow-all-minus-deny sbpl network policy |
| `mur-core/src/cmd/agent/perm.rs` | `set-mode` accepts `broad-audited` (writes authorization + telemetry); `show` prints warning; `deny-host` writes `deny_hosts` |
| `mur-core/src/cmd/agent/import*.rs` | Downgrade `broad-audited`+ → `restricted` on import |
| `mur-core/src/cmd/agent/lifecycle.rs` | Bug 1: alias→model_ref |
| `mur-core/src/cmd/agent/mcp.rs` | Bug 2: `allow_hyphen_values` on `--arg` |
| `mur-core/src/cmd/fleet/*.rs` | Bug 3: `fleet add` comma-split + validation |
| `mur-core/src/cmd/skill_cmd.rs` | Bug 4: `skill new` default `--dir ~/.mur/skills`; Bug 8: better `--fleet` error |
| `mur-core/src/cmd/agent/{import,lifecycle}.rs` | Bug 5: `agent import --as` / clone |
| `mur-core/src/cmd/agent/doctor.rs` (or mod) | Bug 6: per-agent `doctor <name>` |
| `mur-core/src/cmd/agent/restart.rs` | Bug 7: variadic (reconcile with #657 `start`) |
| `mur-core/src/agent_admin/lifecycle.rs` | G2: plist `EnvironmentVariables.PATH` |

---

## Part A — Phase 1: `broad-audited` egress mode

### Task 1: Data model — `BroadAudited` variant + `OutboundNetwork` fields

**Files:**
- Modify: `mur-common/src/agent.rs` (`NetworkOutboundMode` enum ~line 556; `OutboundNetwork` struct ~line 540)
- Test: same file `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `NetworkOutboundMode::BroadAudited`; `OutboundNetwork { deny_hosts: Vec<String>, tool_scope: Option<String>, authorization: Option<EgressAuthorization> }`; `struct EgressAuthorization { mode: NetworkOutboundMode, authorized_by: String, authorized_at_ms: u64 }`. Consumed by Tasks 2–5.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn broad_audited_serde_roundtrip_and_defaults() {
    let ob = OutboundNetwork {
        mode: NetworkOutboundMode::BroadAudited,
        allow_hosts: vec![],
        deny_hosts: vec!["evil.example".into()],
        protocols: default_protocols(),
        resolve_dns: ResolveDnsConfig::default(),
        tool_scope: Some("agent-browser".into()),
        authorization: Some(EgressAuthorization {
            mode: NetworkOutboundMode::BroadAudited,
            authorized_by: "david".into(),
            authorized_at_ms: 1_750_000_000_000,
        }),
    };
    let y = serde_yaml::to_string(&ob).unwrap();
    assert!(y.contains("broad-audited"));
    let back: OutboundNetwork = serde_yaml::from_str(&y).unwrap();
    assert_eq!(back, ob);
    // legacy profiles without the new fields still parse (serde default)
    let legacy: OutboundNetwork =
        serde_yaml::from_str("mode: restricted\nallow_hosts: []\n").unwrap();
    assert_eq!(legacy.deny_hosts, Vec::<String>::new());
    assert!(legacy.authorization.is_none());
}
```

- [ ] **Step 2: Run — expect fail** `cargo test -p mur-common broad_audited_serde_roundtrip -- --nocapture` → FAIL (variant/fields missing).

- [ ] **Step 3: Implement**

Add to the enum:
```rust
pub enum NetworkOutboundMode {
    Unrestricted,
    #[serde(rename = "broad-audited")]
    BroadAudited,
    Restricted,
    Off,
}
```
Add fields to `OutboundNetwork` (all `#[serde(default)]` for back-compat):
```rust
    #[serde(default)]
    pub deny_hosts: Vec<String>,
    #[serde(default)]
    pub tool_scope: Option<String>,
    #[serde(default)]
    pub authorization: Option<EgressAuthorization>,
```
Add the struct:
```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EgressAuthorization {
    pub mode: NetworkOutboundMode,
    pub authorized_by: String,
    pub authorized_at_ms: u64,
}
```
Update any struct-literal constructors of `OutboundNetwork` in the crate (grep `OutboundNetwork {`) to include the new fields (or switch them to `..Default::default()` if a `Default` exists; if not, add the three fields explicitly).

- [ ] **Step 4: Run — expect pass.** Also `cargo build -p mur-common` to catch other construction sites.

- [ ] **Step 5: Commit** `git commit -am "feat(egress): add BroadAudited mode + deny_hosts/tool_scope/authorization to OutboundNetwork"`

---

### Task 2: sbpl enforcement — `BroadAudited` allows all-minus-deny

**Files:**
- Modify: `mur-agent-runtime/src/entitlements.rs` (imports `NetworkOutboundMode`, builds the network policy)
- Test: same file

**Interfaces:**
- Consumes: `NetworkOutboundMode::BroadAudited`, `OutboundNetwork.deny_hosts` (Task 1).
- Produces: sbpl network rules that allow arbitrary hosts except `deny_hosts`.

- [ ] **Step 1: Read the current network-policy builder.** In `entitlements.rs`, find where `NetworkOutboundMode` is matched (the `UnrestrictedNetwork` capability / restricted host-list branch). Note the exact function that emits sbpl `(allow network* ...)` rules.

- [ ] **Step 2: Write the failing test**

```rust
#[test]
fn broad_audited_allows_all_but_denies_listed_hosts() {
    let ob = OutboundNetwork {
        mode: NetworkOutboundMode::BroadAudited,
        allow_hosts: vec![],
        deny_hosts: vec!["blocked.example".into()],
        protocols: default_protocols(),
        resolve_dns: Default::default(),
        tool_scope: None,
        authorization: None,
    };
    let policy = build_network_sbpl(&ob); // name per the actual builder fn
    assert!(policy.allows_arbitrary_outbound());     // e.g. contains an allow-all network rule
    assert!(policy.denies_host("blocked.example"));  // deny overlay present
}
```
(Adapt assertions to the builder's real return type — if it returns an sbpl `String`, assert on the emitted rule text: an unrestricted `(allow network-outbound)` plus a `(deny ... blocked.example)`.)

- [ ] **Step 3: Run — expect fail** `cargo test -p mur-agent-runtime broad_audited_allows_all -- --nocapture`.

- [ ] **Step 4: Implement.** Add a `BroadAudited` match arm alongside `Unrestricted`: emit the same allow-all outbound rules as `Unrestricted`, then append explicit `deny` rules for each `deny_hosts` entry (deny after allow → deny wins in sbpl). Reuse the existing host-matching helper used by `Restricted`/`allow_hosts` to render each deny host.

- [ ] **Step 5: Run — expect pass.** Commit `git commit -am "feat(egress): enforce BroadAudited = allow-all minus deny_hosts in sbpl"`

---

### Task 3: `perm set-mode broad-audited` — consent + authorization record + telemetry

**Files:**
- Modify: `mur-core/src/cmd/agent/perm.rs` (`cmd_perm_set_mode` line 59; add value parse + record)
- Test: `mur-core` test module for perm

**Interfaces:**
- Consumes: `EgressAuthorization` (Task 1).
- Produces: setting mode `broad-audited` writes `outbound.mode = BroadAudited` AND `outbound.authorization = Some(EgressAuthorization{..})`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn set_mode_broad_audited_records_authorization() {
    let dir = tempdir().unwrap();
    let name = seed_test_agent(&dir);        // helper: writes a minimal profile.yaml
    cmd_perm_set_mode(name, "network.outbound", "broad-audited").unwrap();
    let p = load_profile(name).unwrap();
    assert_eq!(p.entitlements.network.outbound.mode, NetworkOutboundMode::BroadAudited);
    let auth = p.entitlements.network.outbound.authorization.expect("record");
    assert_eq!(auth.mode, NetworkOutboundMode::BroadAudited);
    assert!(!auth.authorized_by.is_empty());
    assert!(auth.authorized_at_ms > 0);
}
```

- [ ] **Step 2: Run — expect fail** (`broad-audited` currently unsupported → error).

- [ ] **Step 3: Implement.** In `cmd_perm_set_mode`, the `"network.outbound"` arm parses `value`: extend it to accept `"broad-audited"` → `NetworkOutboundMode::BroadAudited`. When the new mode is `BroadAudited`, set `outbound.authorization = Some(EgressAuthorization { mode: BroadAudited, authorized_by: current_operator(), authorized_at_ms: now_ms() })`. Use the existing operator/user resolution used elsewhere (grep for how `authorized_by`-like values are sourced; fall back to `$USER`). Emit one telemetry event (reuse the crate's telemetry emit; event name `egress.broad_audited.enabled` with the agent name). When switching AWAY from `BroadAudited`, clear `authorization`.

- [ ] **Step 4: Run — expect pass.** Commit `git commit -am "feat(egress): perm set-mode broad-audited records authorization + emits telemetry"`

---

### Task 4: `perm show` prints the BROAD EGRESS warning; `deny-host` persists

**Files:**
- Modify: `mur-core/src/cmd/agent/perm.rs` (`cmd_perm_show` line 33; `cmd_perm_deny_host` line 116; `cmd_perm_list_hosts` line 129)

**Interfaces:**
- Consumes: `OutboundNetwork.mode`, `deny_hosts` (Task 1).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn perm_show_warns_on_broad_audited_and_deny_host_persists() {
    let dir = tempdir().unwrap();
    let name = seed_test_agent(&dir);
    cmd_perm_set_mode(name, "network.outbound", "broad-audited").unwrap();
    cmd_perm_deny_host(name, "blocked.example").unwrap();
    let p = load_profile(name).unwrap();
    assert!(p.entitlements.network.outbound.deny_hosts.iter().any(|h| h == "blocked.example"));
    let out = capture_stdout(|| cmd_perm_show(name, Some("network")).unwrap());
    assert!(out.contains("BROAD EGRESS ON"));
}
```

- [ ] **Step 2: Run — expect fail.**

- [ ] **Step 3: Implement.** (a) `cmd_perm_deny_host`: push the glob into `outbound.deny_hosts` (verify it currently writes nowhere useful — grep; if it wrote to a phantom list, repoint it) and save; `cmd_perm_list_hosts` prints both allow and deny lists. (b) `cmd_perm_show`: when `outbound.mode == BroadAudited`, print a prominent line, e.g. `⚠ BROAD EGRESS ON — this agent may reach any host except its deny_hosts (authorized by <by> at <ts>)`.

- [ ] **Step 4: Run — expect pass.** Commit `git commit -am "feat(egress): perm show warns on broad-audited; deny-host persists to deny_hosts"`

---

### Task 5: Import downgrades `broad-audited`+ to `restricted`

**Files:**
- Modify: the agent import path (`mur-core/src/cmd/agent/import*.rs` or wherever `.muragent` profiles are installed)
- Test: same

**Interfaces:**
- Consumes: `NetworkOutboundMode` (Task 1).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn import_downgrades_broad_and_unrestricted_to_restricted() {
    let profile = profile_with_outbound(NetworkOutboundMode::BroadAudited);
    let installed = sanitize_imported_profile(profile);   // the import hook
    assert_eq!(installed.entitlements.network.outbound.mode, NetworkOutboundMode::Restricted);
    assert!(installed.entitlements.network.outbound.authorization.is_none());
    let u = sanitize_imported_profile(profile_with_outbound(NetworkOutboundMode::Unrestricted));
    assert_eq!(u.entitlements.network.outbound.mode, NetworkOutboundMode::Restricted);
}
```

- [ ] **Step 2: Run — expect fail.**

- [ ] **Step 3: Implement.** In the import/install path, after deserializing the incoming profile, if `outbound.mode` is `BroadAudited` or `Unrestricted`, set it to `Restricted` and clear `authorization`. If no single sanitize function exists, add `fn sanitize_imported_profile(mut p: AgentProfile) -> AgentProfile` and call it at the install boundary. Print a notice: `imported agent's broad egress downgraded to restricted — re-grant locally with 'mur agent perm set-mode'`.

- [ ] **Step 4: Run — expect pass.** Commit `git commit -am "feat(egress): import downgrades broad-audited/unrestricted to restricted (lowest trust)"`

---

## Part B — CLI hardening

### Task 6: Bug 1 — `agent create --model <alias>` sets `model_ref`

**Files:**
- Modify: `mur-core/src/cmd/agent/lifecycle.rs` (`resolve_model_ref_for_create` ~line 218; `cmd_create` ~line 17)
- Test: same

**Interfaces:**
- Consumes: the models.yaml registry loader (already used in `resolve_model_ref_for_create`).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn create_with_bare_alias_sets_model_ref() {
    let home = tempdir().unwrap();
    seed_models_yaml(&home, "claude_sonnet", "anthropic", "claude-sonnet-5");
    let mr = resolve_model_ref_for_create(home.path(), /*provider*/ None, /*model*/ "claude_sonnet").unwrap();
    assert_eq!(mr, Some("claude_sonnet".to_string()));
}
```
(Match the real signature of `resolve_model_ref_for_create`; the point is: a bare value equal to a registry alias yields that alias as `model_ref`.)

- [ ] **Step 2: Run — expect fail** (bare alias currently returns `None` / ollama default).

- [ ] **Step 3: Implement.** In `resolve_model_ref_for_create`, before defaulting provider to `ollama`: if the `--model` value exactly matches an alias key in `~/.mur/models.yaml`, return `Some(alias)` and derive provider/name from that registry entry (so the inline block matches too). Keep the existing reverse-map (`--provider X --model realname`) path.

- [ ] **Step 4: Run — expect pass.** Commit `git commit -am "fix(agent): create --model <alias> resolves to model_ref instead of ollama default"`

---

### Task 7: Bug 2 — `mcp add --arg` accepts `--`-prefixed values

**Files:**
- Modify: `mur-core/src/cmd/agent/mcp.rs` (the clap `--arg` definition)

- [ ] **Step 1: Write/confirm the failing case.** From a shell in the worktree: `cargo run -p mur-core -- agent mcp add t x --command foo --arg --engine 2>&1` → currently `unexpected argument '--engine'`. Record as the failing baseline.

- [ ] **Step 2: Implement.** Add `#[arg(long = "arg", allow_hyphen_values = true)]` (or `.allow_hyphen_values(true)` in the builder) to the `args` option so a value starting with `--` is consumed as the value. Add a help example: `--arg=--engine or --arg --engine`.

- [ ] **Step 3: Verify.** Re-run the Step-1 command → the entry is added with `--engine` stored. Also `cargo test -p mur-core` for no regressions.

- [ ] **Step 4: Commit** `git commit -am "fix(agent): mcp add --arg allows hyphen-prefixed values"`

---

### Task 8: Bug 3 — `fleet add` comma-splits + validates members

**Files:**
- Modify: `mur-core/src/cmd/fleet/` (the `add` handler)
- Test: same

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn fleet_add_splits_commas_and_rejects_unknown() {
    let names = parse_member_args(&["a,b".to_string(), "c".to_string()]);
    assert_eq!(names, vec!["a", "b", "c"]);   // comma + space both split
    // unknown-agent rejection is covered by the add path returning Err for a missing agent
}
```

- [ ] **Step 2: Run — expect fail** (currently `"a,b"` stays one token).

- [ ] **Step 3: Implement.** Add `fn parse_member_args(raw: &[String]) -> Vec<String>` that flat-maps each token on `,`, trims, drops empties. Use it in the `fleet add` handler; then validate each resolved name exists (reuse `a2a_dial::canonicalize_agent_name` / the agent-exists check) and return a clear `Err` listing unknown names before writing membership.

- [ ] **Step 4: Run — expect pass.** Commit `git commit -am "fix(fleet): add splits comma-separated members and validates existence"`

---

### Task 9: Bug 4 + Bug 8 — `skill new` default dir; `skill scope --fleet` error

**Files:**
- Modify: `mur-core/src/cmd/skill_cmd.rs` (`cmd_new` / `NewOptions`; the `scope` handler)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn skill_new_defaults_to_mur_skills_dir() {
    let home = tempdir().unwrap();
    let path = scaffold_skill(NewOptions { name: "t".into(), dir: None, mur_home: Some(home.path().into()), ..default_new_opts() }).unwrap();
    assert!(path.starts_with(home.path().join("skills")));
}
```

- [ ] **Step 2: Run — expect fail** (currently defaults to CWD).

- [ ] **Step 3: Implement.** (Bug 4) In `scaffold_skill`/`cmd_new`, when `dir` is `None`, default the output root to `<mur_home>/skills` instead of CWD. (Bug 8) In the `scope` handler, when `--fleet` is passed without a value / no fleet name resolvable, return `Err("--fleet requires a fleet NAME, e.g. --fleet aura-research; the fleet need not exist yet")`.

- [ ] **Step 4: Run — expect pass.** Commit `git commit -am "fix(skill): new defaults to ~/.mur/skills; scope --fleet error is actionable"`

---

### Task 10: Bug 5 — `agent import --as <name>` / clone

**Files:**
- Modify: `mur-core/src/cmd/agent/import*.rs` + the clap `import` args
- Test: same

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn import_as_renames_and_regenerates_identity() {
    let bundle = export_agent_to_bundle("aura");
    let installed = import_agent(&bundle, /*as_name*/ Some("aura-w1")).unwrap();
    assert_eq!(installed.name, "aura-w1");
    assert!(installed.identity_pub != source_identity("aura")); // fresh identity
    // private key never copied — assert the source key file was not read into the new dir
}
```

- [ ] **Step 2: Run — expect fail** (`--as` unsupported).

- [ ] **Step 3: Implement.** Add `--as <NAME>` to the `import` clap args. In the import handler, when `as_name` is `Some`, install under that directory/name, set `profile.name`, regenerate the agent identity (reuse the create-time identity generation), and NEVER copy the source private key. Refuse if the target name already exists.

- [ ] **Step 4: Run — expect pass.** Commit `git commit -am "feat(agent): import --as clones an agent under a new name with fresh identity"`

---

### Task 11: Bug 6 — per-agent `agent doctor <name>`

**Files:**
- Modify: `mur-core/src/cmd/agent/` doctor handler + clap args
- Test: same

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn agent_doctor_named_checks_model_ref_and_mcp() {
    let name = seed_test_agent_with_bad_model_ref();
    let report = agent_doctor(Some(name)).unwrap();
    assert!(report.iter().any(|c| c.name == "model_ref" && !c.ok));
}
```

- [ ] **Step 2: Run — expect fail** (doctor takes no name arg).

- [ ] **Step 3: Implement.** Make the `doctor` arg an `Option<String>`. With `Some(name)`: run per-agent checks — `model_ref` resolves in models.yaml; each MCP `command` resolves on PATH (reuse `agent_mcp_pin::resolve_command`); entitlements parse. Return a `Vec<Check { name, ok, detail }>`. With `None`: keep the existing export-prereq behavior unchanged.

- [ ] **Step 4: Run — expect pass.** Commit `git commit -am "feat(agent): doctor <name> runs per-agent health checks"`

---

### Task 12: Bug 7 — `agent restart` variadic (reconcile with #657)

**Files:**
- Modify: `mur-core/src/cmd/agent/restart.rs`

- [ ] **Step 1: Check current state.** #657 merged a `mur agent start` subcommand; run `cargo run -p mur-core -- agent restart --help` to see if `restart` already takes `--all`/multiple. If it already accepts multiple names, mark this task done (no change) and note it.

- [ ] **Step 2: Write the failing test (only if still single-name).**

```rust
#[test]
fn restart_accepts_multiple_names() {
    let parsed = RestartArgs::try_parse_from(["restart", "a", "b"]).unwrap();
    assert_eq!(parsed.names, vec!["a", "b"]);
}
```

- [ ] **Step 3: Implement (if needed).** Change the positional from `Option<String>` to `Vec<String>` (variadic), loop the restart over each, keep `--all`/`--stale` mutually exclusive.

- [ ] **Step 4: Run — expect pass (or note no-op).** Commit `git commit -am "fix(agent): restart accepts multiple names"`

---

### Task 13: G2 — `install-service` writes `EnvironmentVariables.PATH`

**Files:**
- Modify: `mur-core/src/agent_admin/lifecycle.rs` (launchd plist template)
- Test: same

**Interfaces:**
- Produces: the generated plist contains an `EnvironmentVariables` dict with a `PATH` including the user's tool bins.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn plist_includes_path_env() {
    let plist = render_launchd_plist("aura", "/path/to/binary", &derive_service_path());
    assert!(plist.contains("<key>EnvironmentVariables</key>"));
    assert!(plist.contains("<key>PATH</key>"));
    assert!(plist.contains("/bin"));   // system dirs present
}
```

- [ ] **Step 2: Run — expect fail** (no EnvironmentVariables in the template).

- [ ] **Step 3: Implement.** Add `fn derive_service_path() -> String` = join of `<npm prefix>/bin` (from `npm config get prefix`, falling back gracefully if npm absent), `/opt/homebrew/bin`, `/usr/local/bin`, and the launchd defaults `/usr/bin:/bin:/usr/sbin:/sbin`. Inject an `EnvironmentVariables` dict with that `PATH` into the plist template. Not hardcoded — derived at generation time.

- [ ] **Step 4: Run — expect pass.** Commit `git commit -am "fix(agent): install-service plist sets EnvironmentVariables.PATH so PATH-installed MCP binaries resolve"`

---

## Roadmap (NOT tasks — future phases)

- **Phase 2 — Audit plane:** structured per-egress event log reduced into a network-wide view; a Commander `revoke-egress` directive honored by runtime + daemon (fail-closed). Reuse the `authorization` records from Phase 1 as the grant ledger.
- **Phase 3 — Managed egress proxy:** DLP inspection, rate limiting, per-request allow/deny; sbpl pins egress to the proxy so it cannot be bypassed.
- **Phase 4 — Per-tool scoping enforcement:** enforce `tool_scope` (added to the schema in Task 1) so only the named MCP subprocess may egress.

---

## Self-Review

**Spec coverage:**
- §2.2 `broad-audited` mode → Tasks 1–4. §2.3 scoping field → Task 1 (`tool_scope`, schema-only). §2.3 Commander → roadmap (Phase 2). §2.3 portability/import downgrade → Task 5. §2.4 data model → Task 1 (authorization on profile, NOT `GovernanceState` — grounded correction, noted in Architecture). §2.6 acceptance → Tasks 2–4. §3 Part B (8 bugs + G2) → Tasks 6–13. §2.5 phasing → Roadmap section.
- Gap check: spec §2.4 said "GovernanceState gains an egress section" — reconciled: `GovernanceState` is ephemeral fleet-loop state, so the persistent record lives on the profile (`EgressAuthorization`) + a telemetry event; a network-wide reduction is Phase 2. This is a deliberate, documented deviation, not a miss.

**Placeholder scan:** No TBD/TODO. Test bodies are concrete; where a real signature must be matched (e.g. `resolve_model_ref_for_create`, the sbpl builder fn), the step says "match the actual signature" and gives the assertion intent — acceptable because the exact name is discoverable in the named file and the behavior is fully specified.

**Type consistency:** `NetworkOutboundMode::BroadAudited`, `OutboundNetwork.{deny_hosts,tool_scope,authorization}`, `EgressAuthorization{mode,authorized_by,authorized_at_ms}` used consistently across Tasks 1–5.
