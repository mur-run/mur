# Loopback-Only Pin-to-Proxy (`ProxyOnly` posture) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the deep-research worker a `ProxyOnly` sandbox network posture — deny all general outbound TCP (`*:port`) while allowing ONLY the loopback carve-outs (its cc-proxy LLM port + the egress proxy port) — so every egress is forced through the (SSRF-screening, IP-pinning) egress proxy, closing the direct-egress bypass #689 couldn't.

**Architecture:** A new `NetworkOutboundMode::ProxyOnly` maps (in `from_entitlements`) to `net_allow_ports = Some([])` (deny general TCP) + `net_allow_hosts = Some(allow_hosts)` (HostGuard still governs the runtime's own client). The port-assembly helpers route the LLM port to the loopback carve-out list under this posture; the macOS SBPL builder's empty-ports branch emits mDNS + `localhost:{port}` carve-outs but no `*:{port}`; Linux Landlock already emits loopback ports under an empty list. Deep-research provision opts the worker into `ProxyOnly`. Validated by a live fleet turn.

**Tech Stack:** Rust (edition 2024). `mur-common` (enum), `mur-agent-runtime` (sandbox policy + SBPL/Landlock), `mur-core` (deep-research provision).

**Spike:** `docs/superpowers/plans/2026-07-11-spike-loopback-only-pin.md` · Builds on #689 (proxy SSRF-screen + IP-pin).

## Global Constraints

- **Do NOT change generic `Restricted` or `Off` or `Unrestricted` behavior.** `ProxyOnly` is a NEW, additive posture; existing modes' `(net_allow_ports, net_allow_hosts)` mappings and HostGuard behavior stay byte-for-byte.
- **`ProxyOnly` must keep `net_allow_hosts = allow_hosts`** (NOT empty like `Off`) — the worker's LLM client resolves `localhost`/`127.0.0.1` via HostGuard, which `Off`'s empty list would block.
- **No `*:port` for a `ProxyOnly` worker.** The SBPL/Landlock profile emits only `localhost:{port}` (macOS) / loopback `NetPort` (Linux) carve-outs + mDNS for name resolution.
- **The LLM port becomes a loopback carve-out** under `ProxyOnly` (it's cc-proxy on 127.0.0.1), not a general `*:port`.
- Fail-safe: the live test (Task 6) is a MERGE GATE — if the worker's LLM or web breaks, do not merge; narrow the posture.
- No new crate deps. Files ≤ 800 lines. Comments English.
- **Build/test env:** `mur-agent-runtime` + `mur-common` need no special env; `mur-core` needs `export ORT_STRATEGY=download; export MUR_WEB_DIST=$HOME/Projects/mur-web/dist`. Add the rustup toolchain to PATH if `cargo` isn't found.

## File Structure

- `mur-common/src/agent.rs` — add `NetworkOutboundMode::ProxyOnly`.
- `mur-agent-runtime/src/sandbox/policy.rs` — `from_entitlements` ProxyOnly arm; relax `allow_loopback_ports`; route LLM port to loopback in `allow_extra_ports` under ProxyOnly.
- `mur-agent-runtime/src/supervisor_runner.rs` — HostGuard ProxyOnly arm.
- `mur-agent-runtime/src/sandbox/macos.rs` — empty-ports SBPL branch emits mDNS + loopback carve-outs.
- `mur-agent-runtime/src/sandbox/linux.rs` — add a test (already emits loopback under empty list).
- `mur-core/src/cmd/deep_research/provision.rs` — set worker `mode = ProxyOnly`.

---

### Task 1: `NetworkOutboundMode::ProxyOnly` + mappings

**Files:**
- Modify: `mur-common/src/agent.rs` (enum)
- Modify: `mur-agent-runtime/src/sandbox/policy.rs` (`from_entitlements` match)
- Modify: `mur-agent-runtime/src/supervisor_runner.rs` (HostGuard match)

**Interfaces:**
- Produces: `NetworkOutboundMode::ProxyOnly`; `from_entitlements` maps it to `(Some(vec![]), Some(ent.network.outbound.allow_hosts.clone()))`; HostGuard maps it identically to `Restricted`.

- [ ] **Step 1: Write the failing test** (in `policy.rs` test module)

```rust
#[test]
fn proxy_only_denies_general_tcp_but_keeps_host_allowlist() {
    let mut ent = crate::sandbox::policy::tests::minimal_entitlements(); // or however sibling tests build Entitlements
    ent.network.outbound.mode = mur_common::agent::NetworkOutboundMode::ProxyOnly;
    ent.network.outbound.allow_hosts = vec!["localhost".into(), "127.0.0.1".into()];
    let policy = SandboxPolicy::from_entitlements(&ent, std::path::Path::new("/tmp/agent"));
    // General TCP denied (empty port list, but PRESENT — not None/unrestricted).
    assert_eq!(policy.net_allow_ports, Some(vec![]));
    // Host allowlist retained (NOT emptied like Off) so the LLM host resolves.
    assert_eq!(policy.net_allow_hosts, Some(vec!["localhost".to_string(), "127.0.0.1".to_string()]));
}
```

(Model `minimal_entitlements()` on how the existing `policy.rs` tests construct an `Entitlements` — grep the test module for the pattern; the existing `ent.network.outbound.mode = ...` tests at lines ~682-732 show it.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-agent-runtime proxy_only_denies_general_tcp`
Expected: FAIL (`ProxyOnly` variant doesn't exist).

- [ ] **Step 3: Add the variant + mappings**

In `mur-common/src/agent.rs`, the `NetworkOutboundMode` enum (line ~599) — add a variant (place it after `Restricted`; keep serde naming consistent with the others — if the enum uses `#[serde(rename_all=...)]` it'll serialize as `proxy_only` / `proxyOnly` accordingly):

```rust
    /// Deny all general outbound TCP; egress is ONLY via loopback proxies
    /// (the agent's cc-proxy LLM port + the egress proxy). Hostnames are still
    /// governed by `allow_hosts` (HostGuard) — unlike `Off`, which blocks all.
    ProxyOnly,
```

In `mur-agent-runtime/src/sandbox/policy.rs` `from_entitlements` match (line ~366-374), add the arm (keep the others unchanged):

```rust
            NetworkOutboundMode::ProxyOnly => {
                // Deny general TCP (empty-but-present list), keep the host
                // allowlist so the runtime's own client can resolve its
                // loopback LLM endpoint. Loopback carve-outs are added by the
                // port-assembly helpers.
                (Some(vec![]), Some(ent.network.outbound.allow_hosts.clone()))
            }
```

In `mur-agent-runtime/src/supervisor_runner.rs` HostGuard match (line ~251-265), add:

```rust
        NetworkOutboundMode::ProxyOnly => {
            // Same host governance as Restricted (allow_hosts drives HostGuard).
            HostGuard::restricted(profile.entitlements.network.outbound.allow_hosts.clone())
        }
```

(Match the exact `HostGuard::restricted(...)` argument shape the `Restricted` arm uses right above it — copy its body.)

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-agent-runtime proxy_only_denies_general_tcp` then `cargo build -p mur-agent-runtime` (catches any OTHER exhaustive match on `NetworkOutboundMode` that now needs the arm — add a `ProxyOnly` arm wherever the compiler flags a non-exhaustive match; there are ~2 production exhaustive matches).
Expected: PASS + builds.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/agent.rs mur-agent-runtime/src/sandbox/policy.rs mur-agent-runtime/src/supervisor_runner.rs
git commit -m "feat(sandbox): NetworkOutboundMode::ProxyOnly (deny general TCP, keep host allowlist)"
```

---

### Task 2: Route loopback ports (explicit posture flag + LLM→loopback)

**Files:**
- Modify: `mur-agent-runtime/src/sandbox/policy.rs` (struct field, `Default`, `from_entitlements`, `allow_loopback_ports`, `allow_extra_ports`)

> **Design note (why a flag, not `Some([])` inference):** `Off` and `ProxyOnly`
> BOTH map to `net_allow_ports = Some(vec![])` — indistinguishable by that field
> alone. Inferring "allow loopback" from an empty-but-present list would wrongly
> re-open loopback under `Off` (the existing `loopback_ports_respect_off_mode`
> test forbids exactly this). So we add an explicit `net_loopback_allowed: bool`
> (Restricted + ProxyOnly → true; Off + Unrestricted → false). The guards fire on
> `Some(non-empty)` **OR** the flag — which keeps the existing Restricted test
> (relies on non-empty) AND the existing Off test (manual construction defaults
> the flag to `false`) passing UNCHANGED, while enabling ProxyOnly's
> `Some([]) + flag` case.

**Interfaces:**
- Consumes: `net_allow_ports: Option<Vec<u16>>`, `net_allow_loopback_ports: Vec<u16>`.
- Produces: new public field `net_loopback_allowed: bool` on `SandboxPolicy` (set by `from_entitlements`); `allow_loopback_ports` accepts ports when `Some(non-empty)` OR `net_loopback_allowed`; `allow_extra_ports` routes the LLM port to `net_allow_loopback_ports` when general list is empty AND `net_loopback_allowed` (ProxyOnly), else to the general list (Restricted), else no-op.

- [ ] **Step 1: Add the struct field + Default + from_entitlements mapping**

In `SandboxPolicy` (after the `net_allow_loopback_ports` field, ~line 53):

```rust
    /// True when the posture permits loopback carve-outs (Restricted +
    /// ProxyOnly) but NOT Off/Unrestricted. This is the signal that
    /// distinguishes a ProxyOnly `net_allow_ports = Some([])` (deny general TCP,
    /// allow the loopback proxies) from an Off `Some([])` (deny everything) —
    /// the two are identical in `net_allow_ports`. Set by `from_entitlements`;
    /// consulted by `allow_extra_ports` / `allow_loopback_ports`.
    pub net_loopback_allowed: bool,
```

In `Default for SandboxPolicy` (alongside `net_allow_loopback_ports: Vec::new(),`, ~line 72) add:

```rust
            net_loopback_allowed: false,
```

In `from_entitlements`, change the tuple binding to also yield the flag (~line 366):

```rust
        let (net_allow_ports, net_allow_hosts, net_loopback_allowed) = match ent.network.outbound.mode {
            NetworkOutboundMode::Unrestricted => (None, None, false),
            NetworkOutboundMode::Restricted => {
                let ports = Some(vec![80u16, 443, 8080, 8443]);
                let hosts = Some(ent.network.outbound.allow_hosts.clone());
                (ports, hosts, true)
            }
            NetworkOutboundMode::ProxyOnly => {
                // Deny general TCP (empty-but-present list), keep the host
                // allowlist so the runtime's own client can resolve its
                // loopback LLM endpoint. Loopback carve-outs are added by the
                // port-assembly helpers; net_loopback_allowed = true is what
                // lets them fire despite the empty general list.
                (Some(vec![]), Some(ent.network.outbound.allow_hosts.clone()), true)
            }
            NetworkOutboundMode::Off => (Some(vec![]), Some(vec![]), false),
        };
```

And add `net_loopback_allowed,` to the returned `SandboxPolicy { ... }` struct literal (next to `net_allow_loopback_ports: Vec::new(),`, ~line 392).

- [ ] **Step 2: Write the failing tests**

```rust
#[test]
fn proxy_only_port_assembly_routes_llm_and_egress_to_loopback() {
    let mut policy = SandboxPolicy::default();
    policy.net_allow_ports = Some(Vec::new()); // ProxyOnly: deny general TCP
    policy.net_loopback_allowed = true;        // …but loopback carve-outs permitted
    // LLM port (cc-proxy) must land in LOOPBACK, not general ports.
    policy.allow_extra_ports(&[8088]);
    // Egress proxy port must be accepted even though general list is empty.
    policy.allow_loopback_ports(&[54321]);
    assert_eq!(policy.net_allow_ports, Some(vec![]), "general TCP stays denied");
    assert!(policy.net_allow_loopback_ports.contains(&8088), "LLM port routed to loopback");
    assert!(policy.net_allow_loopback_ports.contains(&54321), "egress proxy port accepted");
}

#[test]
fn off_mode_port_assembly_stays_empty() {
    // Off is ALSO net_allow_ports = Some([]), but net_loopback_allowed = false
    // (the default) — neither helper may re-open loopback. This is the guard the
    // flag exists for.
    let mut policy = SandboxPolicy::default();
    policy.net_allow_ports = Some(Vec::new());
    // net_loopback_allowed left false
    policy.allow_extra_ports(&[8088]);
    policy.allow_loopback_ports(&[54321]);
    assert!(policy.net_allow_loopback_ports.is_empty(), "Off adds no loopback carve-outs");
}

#[test]
fn restricted_port_assembly_unchanged() {
    // Non-empty general list = generic Restricted: LLM port still goes to
    // net_allow_ports (unchanged behavior).
    let mut policy = SandboxPolicy::default();
    policy.net_allow_ports = Some(vec![80, 443]);
    policy.allow_extra_ports(&[8088]);
    assert!(policy.net_allow_ports.as_ref().unwrap().contains(&8088));
    assert!(!policy.net_allow_loopback_ports.contains(&8088));
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p mur-agent-runtime proxy_only_port_assembly off_mode_port_assembly`
Expected: FAIL to compile first (`net_loopback_allowed` doesn't exist until Step 1 is done — if you did Step 1 already, the ProxyOnly test FAILS because the helpers don't yet consult the flag).

- [ ] **Step 4: Implement the two helpers**

Replace `allow_loopback_ports` (fire on non-empty general list OR the flag; still no-op under `None`/unrestricted and under `Off`):

```rust
    /// Grant loopback-only access to `extra` TCP ports (the egress proxy, and —
    /// under ProxyOnly — the LLM cc-proxy). Fires for Restricted (non-empty
    /// general list) and ProxyOnly (`net_loopback_allowed`); a no-op under Off
    /// (empty list, flag false) and Unrestricted (`None`).
    pub fn allow_loopback_ports(&mut self, extra: &[u16]) {
        let permitted = matches!(&self.net_allow_ports, Some(p) if !p.is_empty())
            || self.net_loopback_allowed;
        if permitted {
            for p in extra {
                if !self.net_allow_loopback_ports.contains(p) {
                    self.net_allow_loopback_ports.push(*p);
                }
            }
        }
    }
```

Replace `allow_extra_ports` (Restricted → general list; ProxyOnly → loopback; Off/Unrestricted → no-op):

```rust
    /// Grant outbound to `extra` TCP ports for the agent's own LLM endpoint.
    /// Under Restricted (non-empty general list) they join the general list
    /// (`*:port`). Under ProxyOnly (general list present-but-empty AND
    /// `net_loopback_allowed`) the LLM is a loopback cc-proxy, so route its port
    /// to the loopback carve-out instead of opening a general `*:port`. No-op
    /// under Off (empty list, flag false) and Unrestricted (`None`).
    pub fn allow_extra_ports(&mut self, extra: &[u16]) {
        match self.net_allow_ports.as_ref().map(|p| p.is_empty()) {
            Some(false) => {
                if let Some(ports) = &mut self.net_allow_ports {
                    for p in extra {
                        if !ports.contains(p) {
                            ports.push(*p);
                        }
                    }
                }
            }
            Some(true) if self.net_loopback_allowed => {
                for p in extra {
                    if !self.net_allow_loopback_ports.contains(p) {
                        self.net_allow_loopback_ports.push(*p);
                    }
                }
            }
            _ => {}
        }
    }
```

- [ ] **Step 5: Run to verify all pass**

Run: `cargo test -p mur-agent-runtime proxy_only_port_assembly off_mode_port_assembly restricted_port_assembly_unchanged` then `cargo test -p mur-agent-runtime sandbox::policy::` (existing port + loopback-guard tests, incl. `allow_extra_ports_adds_llm_port_in_restricted_mode` and `loopback_ports_respect_off_mode` — ALL must still pass UNCHANGED; do NOT edit any existing test).
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-agent-runtime/src/sandbox/policy.rs
git commit -m "feat(sandbox): route LLM+egress ports to loopback under ProxyOnly (net_loopback_allowed flag)"
```

---

### Task 3: macOS SBPL empty-ports branch emits loopback + mDNS (no wildcard)

**Files:**
- Modify: `mur-agent-runtime/src/sandbox/macos.rs` (`build_sbpl_profile`)

**Interfaces:**
- Consumes: `net_allow_ports`, `net_allow_loopback_ports`, `MDNSRESPONDER_SOCKET`, `unix_socket_allow_paths()` (all already in this fn).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn proxy_only_sbpl_allows_loopback_and_dns_but_no_wildcard() {
    let mut policy = SandboxPolicy::default();
    policy.net_allow_ports = Some(Vec::new());       // deny general TCP
    policy.net_allow_loopback_ports = vec![8088, 54321]; // cc-proxy + egress proxy
    let sbpl = build_sbpl_profile(&policy);

    assert!(sbpl.contains("(deny network-outbound)"));
    // loopback carve-outs present…
    assert!(sbpl.contains("(remote tcp \"localhost:8088\")"));
    assert!(sbpl.contains("(remote tcp \"localhost:54321\")"));
    // …name resolution restored (loopback host resolution)…
    assert!(sbpl.contains("/private/var/run/mDNSResponder"));
    // …and NO wildcard-host tcp allow (the escape hatch).
    assert!(!sbpl.contains("(remote tcp \"*:"), "no wildcard tcp allow:\n{sbpl}");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-agent-runtime proxy_only_sbpl_allows_loopback`
Expected: FAIL (empty branch currently emits only `(deny network-outbound)` — no loopback/mDNS).

- [ ] **Step 3: Implement**

Change the empty-ports arm (currently `Some(ports) if ports.is_empty() => { lines.push("(deny network-outbound)"); }`) to also emit mDNS + loopback carve-outs when there are loopback ports (mirroring the `Some(non-empty)` branch's loopback + mDNS emission, but NO `*:{port}`):

```rust
        Some(ports) if ports.is_empty() => {
            lines.push("(deny network-outbound)".to_string());
            // ProxyOnly / loopback-only: no general `*:port`, but still allow
            // name resolution + the loopback carve-outs (cc-proxy LLM + egress
            // proxy) so the worker can reach its proxies. Mirrors the loopback
            // part of the `Some(non-empty)` arm below.
            if !policy.net_allow_loopback_ports.is_empty() {
                lines.push(format!(
                    "(allow network-outbound (remote unix-socket (path-literal \"{MDNSRESPONDER_SOCKET}\")))"
                ));
                for p in unix_socket_allow_paths() {
                    let p = sbpl_escape(&p.to_string_lossy());
                    lines.push(format!(
                        "(allow network-outbound (remote unix-socket (subpath \"{p}\")))"
                    ));
                }
                for port in &policy.net_allow_loopback_ports {
                    lines.push(format!(
                        "(allow network-outbound (remote tcp \"localhost:{port}\"))"
                    ));
                }
            }
        }
```

**Interface note:** `sbpl_escape`, `unix_socket_allow_paths()` and the `MDNSRESPONDER_SOCKET` string are all already used by the `Some(non-empty)` arm in the same fn — reuse them (copy the emission lines from that arm, dropping the `*:{port}` loop). `MDNSRESPONDER_SOCKET` is currently a `const` declared INSIDE the `Some(non-empty)` arm — hoist it to just above the `match` so both arms can reference it (or redeclare it locally in the empty arm; hoisting is cleaner).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-agent-runtime proxy_only_sbpl_allows_loopback` then `cargo test -p mur-agent-runtime macos` (full SBPL test set).
Expected: ALL PASS with NO edits to existing tests. Specifically confirm these pass UNCHANGED — do NOT touch them:
- `off_mode_denies_network` + `off_mode_still_blocks_dns`: both construct Off with `net_allow_ports = Some([])` and EMPTY loopback ports → the emission is guarded on `net_allow_loopback_ports` being non-empty, so Off still emits ONLY `(deny network-outbound)` and no `(allow ...` / no mDNS. (This is the correctness boundary — if your guard is on the flag or on `is_empty()` of the general list instead of on the loopback list being non-empty, Off breaks. Guard on `!policy.net_allow_loopback_ports.is_empty()`.)
- `restricted_loopback_only_policy_has_no_wildcard_tcp_allow` (#689 guard): constructs `net_allow_ports = Some([])` + `net_allow_loopback_ports = vec![58999]`; it asserts `(deny network-outbound)` present AND no `(remote tcp "*:` wildcard. After your change it ALSO emits `(remote tcp "localhost:58999")` — which is NOT a `*:` match — so BOTH its assertions still hold. It passes unchanged; do NOT edit it.

If any existing test actually fails, STOP and report — do not edit a test to make it pass without escalating.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/sandbox/macos.rs
git commit -m "feat(sandbox): SBPL empty-ports branch emits loopback+mDNS (ProxyOnly), no wildcard"
```

---

### Task 4: Linux Landlock — confirm loopback under empty list

**Files:**
- Modify: `mur-agent-runtime/src/sandbox/linux.rs` (test only)

**Interfaces:**
- Consumes: the existing `if let Some(ports) = &policy.net_allow_ports { for port in ports.iter().chain(net_allow_loopback_ports.iter()) { NetPort::new(port, ConnectTcp) } }`.

- [ ] **Step 1: Write the test** (Landlock's `NetPort` is port-only — no host distinction — so an empty general list + loopback ports already yields ConnectTcp rules ONLY for the loopback ports; confirm the code path)

```rust
#[test]
fn proxy_only_linux_gates_to_loopback_ports_only() {
    // Landlock NetPort is port-scoped (no host); with an empty general list and
    // loopback ports set, the ConnectTcp rules cover ONLY those ports — general
    // egress (e.g. 443) is default-denied. This test documents+guards that the
    // empty-general-list + loopback path is handled (the `Some(ports)` arm fires
    // for Some([]) and chains the loopback ports).
    let mut policy = SandboxPolicy::default();
    policy.net_allow_ports = Some(Vec::new());
    policy.net_allow_loopback_ports = vec![8088, 54321];
    // The port set the Landlock builder would gate on = ports ∪ loopback:
    let gated: Vec<u16> = policy
        .net_allow_ports
        .as_ref()
        .unwrap()
        .iter()
        .chain(policy.net_allow_loopback_ports.iter())
        .copied()
        .collect();
    assert_eq!(gated, vec![8088u16, 54321], "only loopback ports gated, no 443");
}
```

(If `build`/`apply` on Linux isn't callable in a unit test off-Linux, keep this as a pure assertion over the port-set logic the builder uses — it documents the invariant without needing Landlock. If a Linux-only integration path is testable, prefer it under `#[cfg(target_os = "linux")]`.)

- [ ] **Step 2: Run**

Run: `cargo test -p mur-agent-runtime proxy_only_linux_gates`
Expected: PASS (this codifies existing behavior — Linux already handles it; the test is a guard).

- [ ] **Step 3: Commit**

```bash
git add mur-agent-runtime/src/sandbox/linux.rs
git commit -m "test(sandbox): guard Linux gates to loopback ports only under ProxyOnly"
```

---

### Task 5: Deep-research provision opts the worker into `ProxyOnly`

**Files:**
- Modify: `mur-core/src/cmd/deep_research/provision.rs`

**Interfaces:**
- Consumes: `mur_common::agent::NetworkOutboundMode::ProxyOnly`; the existing `profile.entitlements.network.outbound` setup (line ~136-143 sets `allow_hosts = WORKER_LLM_ALLOW_HOSTS`).

- [ ] **Step 1: Write the failing test** (extend/mirror the existing `provision_creates_restricted_workers_with_gateway` assertions at ~388)

```rust
#[test]
fn provision_sets_proxy_only_outbound_mode() {
    // (reuse the existing provision test harness — MUR_HOME/MUR_AGENT_BIN_DIR +
    // seed_models_yaml, exactly as provision_creates_restricted_workers_with_gateway does)
    // … provision one worker …
    assert_eq!(
        p.entitlements.network.outbound.mode,
        mur_common::agent::NetworkOutboundMode::ProxyOnly,
        "deep-research worker is ProxyOnly (egress forced through the proxy)"
    );
    // allow_hosts still seeded with loopback so the LLM host resolves.
    assert!(p.entitlements.network.outbound.allow_hosts.contains(&"127.0.0.1".to_string()));
}
```

(The existing test at ~388 currently asserts `Restricted` — that assertion must FLIP to `ProxyOnly` in this task, or add this new test and update the old one.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core provision_sets_proxy_only`  (with mur-core env)
Expected: FAIL (mode is still `Restricted`).

- [ ] **Step 3: Implement**

In `provision.rs`, where the worker profile's outbound is configured (right after `allow_hosts = WORKER_LLM_ALLOW_HOSTS`, ~line 143), set the mode:

```rust
        // ProxyOnly: deny all general outbound TCP; the worker's egress is
        // entirely loopback (cc-proxy LLM + the audited egress proxy), so the
        // OS profile forces every fetch through the proxy — no direct `*:443`
        // escape. (allow_hosts above keeps HostGuard governing the loopback
        // LLM hostname.)
        profile.entitlements.network.outbound.mode =
            mur_common::agent::NetworkOutboundMode::ProxyOnly;
```

Update the existing `provision_creates_restricted_workers_with_gateway` assertion (~388) from `Restricted` to `ProxyOnly`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-core deep_research::provision`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/deep_research/provision.rs
git commit -m "feat(deep-research): provision workers ProxyOnly (loopback-only egress)"
```

---

### Task 6: Live-test validation (MERGE GATE — controller-run)

**Not a code task.** After Tasks 1–5 are merged-ready, the controller runs a live validation before merge. Rebuild + install the runtime + gateway from the branch, provision a worker, and confirm ALL THREE:

- [ ] **1. LLM works:** a `mur agent send`/research turn completes (worker reaches cc-proxy on loopback → can reason).
- [ ] **2. Web works:** a `fetch`/`search` succeeds through the egress proxy (loopback reachable; `egress proxy CONNECT ALLOW` in the audit).
- [ ] **3. Direct egress DENIED:** from inside the worker sandbox, a DIRECT connect to `example.com:443` (bypassing the proxy) now FAILS (`Operation not permitted` / connect refused). Compare to pre-change, where it would succeed. **This is the airtight proof.** A concrete probe: temporarily point a gateway fetch at a direct (non-proxied) path, or inspect that the emitted SBPL for the running worker has no `(remote tcp "*:443")`.

**If (1) or (2) fails** → the worker needs an egress path we removed → do NOT merge; investigate what direct connection is needed and either add it as a loopback carve-out or narrow the posture. **If (3) still succeeds** → the `*:port` escape isn't closed → the SBPL/assembly change didn't take effect; debug before merge.

---

## Self-Review

**Spec coverage:** spike change 1 (guard relax) → T2; change 2 (empty-branch loopback+mDNS) → T3 (macOS) + T4 (Linux confirm); change 3 (ProxyOnly worker posture) → T1 (the mode + mappings) + T5 (provision). The LLM-port-to-loopback wrinkle the spike under-counted → T2. Live-test → T6. ✓

**Placeholder scan:** `minimal_entitlements()` (T1) and the provision test harness (T5) are "reuse the existing sibling-test constructor" instructions with the exact existing tests named (lines ~682-732, ~388) — concrete, not vague. T6 is explicitly a controller-run validation gate, not a code placeholder. No code-logic placeholders.

**Type consistency:** `NetworkOutboundMode::ProxyOnly` (T1) consumed by `from_entitlements`/HostGuard (T1) + provision (T5). `net_allow_ports: Option<Vec<u16>>` / `net_allow_loopback_ports: Vec<u16>` used consistently in T2/T3/T4. `from_entitlements` ProxyOnly → `(Some(vec![]), Some(allow_hosts))` matches T2's assembly assumption (`Some([])` triggers loopback routing) and T3's SBPL branch (`Some(ports) if ports.is_empty()`).

**Cross-task note (resolved):** T3's empty-ports branch emits carve-outs only when `net_allow_loopback_ports` is non-empty, so Off (empty loopback) still emits deny-only and the `off_mode_*` tests pass unchanged. The #689 guard test (`restricted_loopback_only_policy_has_no_wildcard_tcp_allow`) only asserts "no `*:` wildcard" (NOT "no `localhost:` line"), so it also passes unchanged after the new `localhost:58999` carve-out. No existing test needs editing — verified against the real test bodies. Called out inline in T3 Step 4.
