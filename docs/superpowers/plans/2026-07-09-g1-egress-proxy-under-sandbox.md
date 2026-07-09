# G1: Egress Proxy Reachability Under the B1 Kernel Sandbox — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make per-server MCP egress (`Restricted` / `BroadAudited`, PR #661/#663) actually work under the enforced B1 kernel sandbox, by starting the loopback egress proxy BEFORE the sandbox seals and carving its port into the sandbox profile — unblocking live deep-research web access.

**Architecture:** No new subsystem. Today `supervisor::entrypoint()` seals the sandbox early (with `extra_ports` = LLM port + optional VLC port), and `build_provider_runner()` starts the egress proxy LATER on an ephemeral port — so sandboxed MCP children can never dial the proxy their `HTTPS_PROXY` env points at. The fix follows the existing LLM-port precedent: (1) a loopback-scoped port carve-out in `SandboxPolicy` (macOS SBPL supports `remote tcp "localhost:PORT"`), (2) start the proxy in `entrypoint()` pre-seal and thread the handle into `build_provider_runner()`.

**Tech Stack:** Rust (edition 2024), `mur-agent-runtime` only. No new dependencies.

## Root Cause (empirically pinned, 2026-07-09 live fleet run)

Every gateway `fetch` under a sandboxed worker died with reqwest's generic
`error sending request`, while the SAME binary run standalone fetched
`https://example.com/` fine (200 + content). Chain:

1. `proxy_env_for` (`protocol/mcp_client.rs:345`) hands the gateway child
   `HTTPS_PROXY=http://<token>:x@127.0.0.1:<ephemeral>` — the in-runtime
   egress proxy started by `build_provider_runner`
   (`supervisor_runner.rs:272`) on a random port.
2. The gateway's `reqwest::Client::builder()` (fetcher.rs:66) honors env
   proxies by default → it dials `127.0.0.1:<ephemeral>`.
3. The SBPL profile — sealed earlier in `supervisor::entrypoint()`
   (`supervisor.rs:260`) — allows outbound TCP only on 80/443/8080/8443
   (`policy.rs:334`, Restricted mode) plus `extra_ports` (LLM port,
   `supervisor.rs:223`). The ephemeral proxy port is never included → the
   child's `connect()` to the proxy is denied.
4. Result: the proxy sees NO CONNECT (zero `egress proxy CONNECT` log lines
   all run), and the `BroadAudited` grant is dead on arrival. DNS and the
   gateway's SSRF guard both pass before the failing dial (guard verified
   live: loopback fetch → `PrivateAddress` block).

NOT the root cause (ruled out live): kernel host-level blocking (SBPL is
port-based; 443 is allowed to `*`), agent-level `allow_hosts` (adding
`example.com` + restart changed nothing — that list feeds HostGuard/proxy
policy, not the kernel), DNS (mDNSResponder socket is carved out).

## Global Constraints

- **Fail-closed semantics preserved:** `Off` mode (`net_allow_ports == Some([])`) means the user explicitly denied all outbound — the proxy-port carve-out must NOT widen it (same rule as `allow_extra_ports`, `policy.rs:363`).
- **Loopback-scoped on macOS:** the proxy-port rule must be `(allow network-outbound (remote tcp "localhost:{port}"))` — NOT `"*:{port}"`. SBPL accepts only `*` or `localhost` as host (`macos.rs:228` comment); use `localhost`.
- **Landlock is port-only:** on Linux the same port is added as a `NetPort::new(port, AccessNet::ConnectTcp)` rule (host scoping impossible) — document, don't fake.
- **Single behavior change:** proxy starts earlier and its port is reachable. Registration tokens, per-server policies, `proxy_env_for`, and the on-failure "unscoped" fallback keep today's semantics.
- No hardcoded values; single source file ≤ 800 lines; `cargo clippy --workspace -- -D warnings` + `cargo fmt --check` clean.
- Test with `export ORT_STRATEGY=download`; plain `cargo test -p mur-agent-runtime <filter>`.

---

### Task 1: Loopback-port carve-out in `SandboxPolicy` + platform emits

**Files:**
- Modify: `mur-agent-runtime/src/sandbox/policy.rs` (struct + Default + setter, near `allow_extra_ports` at line ~367)
- Modify: `mur-agent-runtime/src/sandbox/macos.rs` (emit in the `Some(ports)` non-empty arm, after the existing port loop at line ~262)
- Modify: `mur-agent-runtime/src/sandbox/linux.rs` (NetPort rules alongside `net_allow_ports`, line ~51)

**Interfaces:**
- Produces: `SandboxPolicy.net_allow_loopback_ports: Vec<u16>` (default empty) and `SandboxPolicy::allow_loopback_ports(&mut self, extra: &[u16])`. Task 2 consumes the setter via `sandbox::apply`.

- [ ] **Step 1: Write the failing tests** (append to the existing `#[cfg(test)] mod tests` in `macos.rs` and `policy.rs`)

`macos.rs` tests:

```rust
    #[test]
    fn loopback_port_carveout_is_localhost_scoped() {
        let mut policy = SandboxPolicy {
            net_allow_ports: Some(vec![80, 443]),
            ..Default::default()
        };
        policy.allow_loopback_ports(&[54321]);
        let sbpl = build_sbpl_profile(&policy);
        assert!(
            sbpl.contains("(allow network-outbound (remote tcp \"localhost:54321\"))"),
            "proxy port must be loopback-scoped: {sbpl}"
        );
        assert!(
            !sbpl.contains("\"*:54321\""),
            "proxy port must NOT be wildcard-host: {sbpl}"
        );
    }
```

`policy.rs` tests:

```rust
    #[test]
    fn loopback_ports_respect_off_mode() {
        // Off = user denied all outbound; the carve-out must not reopen it.
        let mut p_off = SandboxPolicy {
            net_allow_ports: Some(vec![]),
            ..Default::default()
        };
        p_off.allow_loopback_ports(&[54321]);
        assert!(p_off.net_allow_loopback_ports.is_empty());

        // Restricted: carve-out applies, deduplicated.
        let mut p_r = SandboxPolicy {
            net_allow_ports: Some(vec![80, 443, 8080, 8443]),
            ..Default::default()
        };
        p_r.allow_loopback_ports(&[54321]);
        p_r.allow_loopback_ports(&[54321]);
        assert_eq!(p_r.net_allow_loopback_ports, vec![54321]);

        // Unrestricted (None): (allow default) already covers it; no rule needed.
        let mut p_u = SandboxPolicy {
            net_allow_ports: None,
            ..Default::default()
        };
        p_u.allow_loopback_ports(&[54321]);
        assert!(p_u.net_allow_loopback_ports.is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-agent-runtime loopback_port`
Expected: compile FAIL — `net_allow_loopback_ports` / `allow_loopback_ports` not defined.

- [ ] **Step 3: Implement the policy field + setter**

In `policy.rs`, add to the `SandboxPolicy` struct (next to `net_allow_ports`, line ~47) — and add `net_allow_loopback_ports: Vec::new()` to the `Default` impl (line ~65):

```rust
    /// Loopback-only TCP port carve-outs (e.g. the in-runtime egress proxy's
    /// listener). Emitted as `remote tcp "localhost:{port}"` on macOS SBPL;
    /// on Linux Landlock (port-only, no host scoping) as a plain
    /// `NetPort ConnectTcp` rule. Only populated in Restricted mode — see
    /// `allow_loopback_ports`.
    pub net_allow_loopback_ports: Vec<u16>,
```

Add the setter right after `allow_extra_ports` (line ~367), mirroring its Off-mode guard:

```rust
    /// Carve out loopback-only TCP ports (e.g. the egress proxy listener,
    /// which sandboxed MCP children must dial via `HTTPS_PROXY`).
    ///
    /// Same fail-closed rule as [`Self::allow_extra_ports`]: only applies in
    /// *Restricted* mode. `None` (Unrestricted) already allows everything and
    /// `Some([])` (Off) means the user explicitly denied all outbound TCP —
    /// we respect that and do not silently re-open it.
    pub fn allow_loopback_ports(&mut self, extra: &[u16]) {
        if let Some(ports) = &self.net_allow_ports
            && !ports.is_empty()
        {
            for p in extra {
                if !self.net_allow_loopback_ports.contains(p) {
                    self.net_allow_loopback_ports.push(*p);
                }
            }
        }
    }
```

- [ ] **Step 4: Emit the SBPL rule (macos.rs)**

In `build_sbpl_profile`, inside the `Some(ports)` (non-empty) arm, immediately after the existing `for port in ports { ... "*:{port}" ... }` loop (line ~262):

```rust
            // Loopback-only carve-outs (egress proxy listener): SBPL's
            // `remote tcp` accepts `localhost` as the host, so this does NOT
            // widen general egress — only dials to 127.0.0.1/::1 on the port.
            for port in &policy.net_allow_loopback_ports {
                lines.push(format!(
                    "(allow network-outbound (remote tcp \"localhost:{port}\"))"
                ));
            }
```

- [ ] **Step 5: Emit the Landlock rule (linux.rs)**

In the `if let Some(ports) = &policy.net_allow_ports` block (line ~51), extend the rule loop to also cover the loopback ports (Landlock `NetPort` cannot scope by host — the port-wide grant is the closest primitive; the doc comment on the policy field says so):

```rust
        for port in ports.iter().chain(policy.net_allow_loopback_ports.iter()) {
            ruleset = ruleset
                .add_rule(NetPort::new(*port, AccessNet::ConnectTcp))?;
        }
```

(Adapt to the surrounding loop's exact shape — keep whatever error-handling idiom the existing `add_rule` call uses; the only change is chaining `net_allow_loopback_ports` into the iteration.)

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p mur-agent-runtime loopback_port`
Expected: both new tests PASS.

Run: `cargo test -p mur-agent-runtime sandbox` and `cargo clippy -p mur-agent-runtime -- -D warnings`
Expected: all pre-existing sandbox tests PASS, clippy clean.

- [ ] **Step 7: Commit**

```bash
git add mur-agent-runtime/src/sandbox/policy.rs mur-agent-runtime/src/sandbox/macos.rs mur-agent-runtime/src/sandbox/linux.rs
git commit -m "feat(sandbox): loopback-only port carve-out in SandboxPolicy (SBPL localhost / Landlock NetPort)"
```

---

### Task 2: Start the egress proxy pre-seal and thread the handle

**Files:**
- Modify: `mur-agent-runtime/src/supervisor.rs` (entrypoint: start proxy before `sandbox::apply` at line ~260; pass handle at the `build_provider_runner` call, line ~333)
- Modify: `mur-agent-runtime/src/sandbox/mod.rs` (`apply` signature, line ~35)
- Modify: `mur-agent-runtime/src/supervisor_runner.rs` (`build_provider_runner` gains `egress_proxy` param; delete its internal proxy-start block at lines ~262-283; extract `needs_egress` into a testable helper)

**Interfaces:**
- Consumes: `SandboxPolicy::allow_loopback_ports` (Task 1), `EgressProxyHandle { pub addr: SocketAddr, .. }` (existing, Clone).
- Produces: `sandbox::apply(entitlements, agent_home, extra_ports, loopback_ports, extra_write_paths)` (new 4th param) and `build_provider_runner(.., egress_proxy: Option<EgressProxyHandle>, ..)` (new param, inserted after `profile`); `pub(crate) fn profile_needs_egress(entries: &[McpServerEntry]) -> bool` in `supervisor_runner.rs`.

- [ ] **Step 1: Write the failing test** (in `supervisor_runner.rs` tests)

```rust
    #[test]
    fn profile_needs_egress_matches_scoped_modes() {
        use mur_common::agent::{McpNetMode, McpServerEntry, McpServerNetwork};
        fn entry(mode: Option<McpNetMode>) -> McpServerEntry {
            let mut e = McpServerEntry::new_for_test("s", "cmd");
            e.network = mode.map(|m| McpServerNetwork {
                mode: m,
                ..Default::default()
            });
            e
        }
        assert!(!profile_needs_egress(&[entry(None)]));
        assert!(!profile_needs_egress(&[entry(Some(McpNetMode::Inherit))]));
        assert!(!profile_needs_egress(&[entry(Some(McpNetMode::Off))]));
        assert!(profile_needs_egress(&[entry(Some(McpNetMode::Restricted))]));
        assert!(profile_needs_egress(&[
            entry(None),
            entry(Some(McpNetMode::BroadAudited))
        ]));
    }
```

If `McpServerEntry` has no `new_for_test` constructor, build the struct literally the way existing `supervisor_runner.rs` / `mcp` tests construct entries — copy that idiom; do not add a new constructor to mur-common just for this.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-agent-runtime profile_needs_egress`
Expected: compile FAIL — `profile_needs_egress` not defined.

- [ ] **Step 3: Extract the helper + rewire `build_provider_runner`**

In `supervisor_runner.rs`, add near the top of the file (module level):

```rust
/// True when any enabled MCP server declares a scoped network policy
/// (`Restricted` / `BroadAudited`) — i.e. the loopback egress proxy is
/// needed. Called by `supervisor::entrypoint()` BEFORE the kernel sandbox
/// seals, so the proxy's listener port can be carved into the profile
/// (a post-seal ephemeral port is unreachable to sandboxed children —
/// the G1 root cause).
pub(crate) fn profile_needs_egress(entries: &[mur_common::agent::McpServerEntry]) -> bool {
    entries.iter().any(|e| {
        matches!(
            e.network.as_ref().map(|n| n.mode),
            Some(mur_common::agent::McpNetMode::Restricted)
                | Some(mur_common::agent::McpNetMode::BroadAudited)
        )
    })
}
```

In `build_provider_runner` (line ~182): add parameter `egress_proxy: Option<crate::sandbox::egress_proxy::EgressProxyHandle>,` immediately after `profile: &Profile,`. Replace the whole internal proxy-start block (lines ~262-283, from the `let needs_egress = ...` comment through `};`) with:

```rust
    // The egress proxy (if any) was started by supervisor::entrypoint()
    // BEFORE the kernel sandbox sealed, so its port is carved into the
    // profile and sandboxed children can dial it. See profile_needs_egress.
    let egress = egress_proxy;
```

- [ ] **Step 4: Extend `sandbox::apply` and start the proxy in `entrypoint`**

`sandbox/mod.rs` — `apply` gains a `loopback_ports: &[u16]` param (insert after `extra_ports`) and applies it:

```rust
pub fn apply(
    entitlements: &Entitlements,
    agent_home: &Path,
    extra_ports: &[u16],
    loopback_ports: &[u16],
    extra_write_paths: &[std::path::PathBuf],
) -> anyhow::Result<SandboxStatus> {
    let mut policy = SandboxPolicy::from_entitlements(entitlements, agent_home);
    // An agent must always be able to reach its own configured local LLM.
    policy.allow_extra_ports(extra_ports);
    // …and the pre-seal loopback egress proxy its MCP children dial.
    policy.allow_loopback_ports(loopback_ports);
    // …and to write the shared runtime media state it owns (co-watching:
    // watch.json + VLC snapshot dir), which lives outside agent_home.
    policy.allow_extra_write_paths(extra_write_paths);
    let status = apply_policy(&policy)?;
    // Store for attestation. OnceLock: if called twice, second call is ignored.
    let _ = SANDBOX_STATUS.set(status.clone());
    Ok(status)
}
```

`supervisor.rs` — immediately BEFORE the `match crate::sandbox::apply(` call (line ~260), insert:

```rust
    // G1: the loopback egress proxy must exist BEFORE the sandbox seals so
    // its listener port can be carved into the kernel profile. Started
    // post-seal (the old order), the ephemeral port is unreachable to
    // sandboxed MCP children and every scoped egress grant is dead on
    // arrival — proven live 2026-07-09 (deep-research workers: zero
    // CONNECTs reached the proxy; standalone gateway fetch worked).
    let egress_proxy = if crate::supervisor_runner::profile_needs_egress(
        &profile.inner.enabled_mcp_servers(),
    ) {
        match crate::sandbox::egress_proxy::start_egress_proxy().await {
            Ok(h) => {
                tracing::info!(addr = %h.addr, "egress proxy started (pre-sandbox)");
                Some(h)
            }
            Err(e) => {
                tracing::warn!(
                    "egress proxy failed to start; scoped MCP servers will be unscoped: {e}"
                );
                None
            }
        }
    } else {
        None
    };
    let loopback_ports: Vec<u16> =
        egress_proxy.iter().map(|h| h.addr.port()).collect();
```

Then add `&loopback_ports,` as the new 4th argument of the `crate::sandbox::apply(` call, and add `egress_proxy.clone(),` (or move it, if entrypoint doesn't use it afterwards) as the new argument after `&profile` at the `build_provider_runner` call (line ~333).

Note: `enabled_mcp_servers()` returns whatever collection type it returns today (`supervisor_runner.rs:260` calls it already) — if it returns `Vec<McpServerEntry>` by value, bind it to a local first and pass a slice; match the existing call's usage.

- [ ] **Step 5: Run tests**

Run: `cargo test -p mur-agent-runtime profile_needs_egress && cargo test -p mur-agent-runtime egress`
Expected: new test PASS; all pre-existing egress_proxy tests PASS (proxy behavior itself unchanged).

Run: `cargo clippy -p mur-agent-runtime -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add mur-agent-runtime/src/supervisor.rs mur-agent-runtime/src/supervisor_runner.rs mur-agent-runtime/src/sandbox/mod.rs
git commit -m "fix(sandbox): start egress proxy pre-seal + carve its port into the kernel profile (G1)"
```

---

### Task 3: Documentation

**Files:**
- Modify: `docs/architecture/runtime-overview.md` — in the per-server MCP egress section, add: "The loopback egress proxy starts **before** the B1 kernel sandbox seals, and its listener port is carved into the sandbox profile as a loopback-only rule (`remote tcp "localhost:PORT"` on macOS SBPL; a port-scoped `NetPort ConnectTcp` rule on Linux Landlock, which cannot scope by host) — otherwise sandboxed MCP children could not dial the proxy their `HTTPS_PROXY` points at and every scoped grant would be silently dead."
- Modify: `docs/superpowers/specs/2026-07-09-mur-native-deep-research-design.md` — in §7 (egress choke point), one sentence noting the same ordering constraint and that it was root-caused in the 2026-07-09 live fleet run.

- [ ] **Step 1: Make both edits** (verbatim sentences above, placed inside the existing sections — do not create new sections)

- [ ] **Step 2: Commit**

```bash
git add docs/architecture/runtime-overview.md docs/superpowers/specs/2026-07-09-mur-native-deep-research-design.md
git commit -m "docs(sandbox): document pre-seal egress proxy start + loopback port carve-out"
```

---

## Operator Verification (manual, after merge)

1. Rebuild + reinstall the runtime binary the worker symlinks point at (`cargo build --release -p mur-agent-runtime`; dr_worker symlinks → `target/release/mur-agent-runtime`).
2. `mur agent start dr_worker_1` (already provisioned + BroadAudited-granted). Expect the new `egress proxy started (pre-sandbox)` info log in stderr.
3. Headless: `mur agent send dr_worker_1 '…fetch https://example.com/ render false…'`.
   **Expected (fixed):** 200 + "Example Domain" text; worker stderr shows `egress proxy CONNECT ALLOW host=example.com`.
   **Old behavior:** `fetch failed: error sending request`, zero CONNECT logs.
4. Negative checks still hold: fetch `http://127.0.0.1:8088/health` → `PrivateAddress` SSRF block; a `--deny-host`-listed host → proxy `CONNECT DENY` + 403.
5. Live fleet re-run: `mur deep-research run deep-research` — workers now return real researched content. (Search tier still fails until G2 — `spawn agent-browser` under sandbox — and convergence still blocked by G3 channel writes; both out of scope here.)

## Out of Scope (tracked separately)

- **G2** gateway search tier (`spawn agent-browser: Operation not permitted`) — browserless HTTP search tier or sandbox exec grant.
- **G3** sandboxed members can't write the shared channel DB (v3d-2 self-reply append) — needs a channel-write path decision (fs grant vs write-through-dialer).
- **G4** skill loader rejects `fleet:<name>` scoped refs — loader validation fix.

## Self-Review Notes

- Fail-closed: Off-mode guard tested (`loopback_ports_respect_off_mode`); on proxy-start failure the fallback is today's exact behavior (warn + unscoped children under the port-gated kernel profile).
- Type consistency: `allow_loopback_ports(&mut self, extra: &[u16])` produced in Task 1, consumed in Task 2's `apply`; `EgressProxyHandle.addr: SocketAddr` (public field, existing) supplies the port; `profile_needs_egress` name used consistently in Task 2 test/impl/callsite.
- The moved proxy start keeps the identical `matches!` predicate (extracted, not rewritten) and the identical warn-and-continue error path.
