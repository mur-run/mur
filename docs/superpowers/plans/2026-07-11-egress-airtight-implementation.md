# Airtight Egress (Proxy SSRF-Screen + IP-Pin) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the egress proxy's SSRF hole and DNS-rebinding window by screening the CONNECT target's resolved IP and connecting to the pinned `SocketAddr` — so a `BroadAudited` grant (esp. a browser-rendered page's sub-resource CONNECT) can't reach the cloud-metadata / link-local endpoint, without a fragile SBPL change and without breaking legitimate local-first (loopback/LAN) egress.

**Architecture:** In `mur-agent-runtime/src/sandbox/egress_proxy.rs`, before `TcpStream::connect(target)`, resolve the CONNECT host once, drop any resolved IP the runtime's existing `reqwest_guard` screen rejects (link-local + unspecified + IPv4-in-IPv6 metadata forms), and connect to the first surviving `SocketAddr` (pinning the IP, so no re-resolution / rebinding). If every resolved IP is screened out → `403` + audit `DENY reason=ssrf`. Plus a guard test that the deep-research worker's emitted SBPL has no general `*:port` allow (the OS-layer invariant the airtight guarantee assumes). No `sandbox_init`/SBPL logic change.

**Tech Stack:** Rust (edition 2024), `tokio::net::TcpStream`, `std::net::{IpAddr, SocketAddr, ToSocketAddrs}`, the existing `mur-agent-runtime/src/sandbox/reqwest_guard.rs`.

**Spike:** `docs/superpowers/plans/2026-07-11-spike-egress-airtight.md`

## Global Constraints

- **Do NOT block loopback/private LAN.** This is a local-first platform that
  legitimately reaches `127.0.0.1` Ollama + LAN LLM endpoints (see the comment on
  `is_link_local_or_unspecified` in `reqwest_guard.rs`). The screen blocks ONLY
  the genuine SSRF target — **link-local (169.254/16, fe80::/10) + unspecified
  (0.0.0.0, ::) + their IPv4-in-IPv6 forms** — reusing the runtime's existing
  `is_link_local_or_unspecified`. Full private/loopback blocking is a separate
  policy decision (conflicts with local-first) and is OUT OF SCOPE.
- **No SBPL / `sandbox_init` / Landlock change.** The fix is entirely in
  `egress_proxy.rs` (+ making one existing helper reusable). The OS layer already
  denies direct egress for the restricted worker.
- **IP-pin:** connect to the screened `SocketAddr`, never re-resolve the hostname
  string — closes the rebinding window at the proxy's connect.
- **Fail-closed:** all-IPs-screened-out → `403 Forbidden` + audit, no upstream
  connection opened.
- No new crate deps. Files ≤ 800 lines. Comments English.
- **Build/test env:** mur-agent-runtime needs none of the mur-core env vars;
  `cargo test -p mur-agent-runtime` works directly (add the rustup toolchain to
  PATH if `cargo` isn't found: `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"`).

## File Structure

- Modify `mur-agent-runtime/src/sandbox/reqwest_guard.rs` — make
  `is_link_local_or_unspecified` reusable by the proxy (`pub(crate)`), and add a
  small `pub(crate) fn screened_socket_addrs(target: &str) -> std::io::Result<Vec<SocketAddr>>`
  that resolves + filters (so the proxy calls one tested helper).
- Modify `mur-agent-runtime/src/sandbox/egress_proxy.rs` — use the helper in
  `handle_conn`: screen + pin before connecting; `403`+audit on empty.
- Modify `mur-agent-runtime/src/sandbox/macos.rs` test module (or wherever the
  deep-research policy SBPL is testable) — a guard test for the no-`*:port`
  invariant. (If the deep-research net policy isn't reachable there, put the
  invariant test in `mur-core/src/cmd/deep_research/provision.rs` over the
  built policy — see Task 3.)

---

### Task 1: Reusable resolve-and-screen helper

**Files:**
- Modify: `mur-agent-runtime/src/sandbox/reqwest_guard.rs`

**Interfaces:**
- Produces: `pub(crate) fn is_link_local_or_unspecified(ip: IpAddr) -> bool` (change visibility from private to `pub(crate)`); `pub(crate) fn screened_socket_addrs(target: &str) -> std::io::Result<Vec<SocketAddr>>` — resolves `target` (`host:port`) via the OS resolver and returns only the addresses that pass the screen (link-local/unspecified dropped).

- [ ] **Step 1: Write the failing test**

Add to `reqwest_guard.rs`'s test module:

```rust
#[test]
fn screened_socket_addrs_drops_link_local_keeps_public() {
    // Loopback is a legitimate local-first target and must be KEPT.
    let lo = screened_socket_addrs("127.0.0.1:443").unwrap();
    assert!(lo.iter().all(|sa| sa.ip().to_string() == "127.0.0.1"));
    assert_eq!(lo.len(), 1);

    // A link-local IP literal (cloud-metadata) must be dropped → empty.
    let meta = screened_socket_addrs("169.254.169.254:80").unwrap();
    assert!(meta.is_empty(), "link-local metadata IP must be screened out");

    // Unspecified dropped too.
    let unspec = screened_socket_addrs("0.0.0.0:80").unwrap();
    assert!(unspec.is_empty());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-agent-runtime reqwest_guard::tests::screened_socket_addrs -- --nocapture`
Expected: FAIL (`screened_socket_addrs` undefined).

- [ ] **Step 3: Implement**

Change `fn is_link_local_or_unspecified` to `pub(crate) fn is_link_local_or_unspecified` (visibility only — body unchanged). Add:

```rust
use std::net::{SocketAddr, ToSocketAddrs};

/// Resolve `target` (`host:port`, or an IP-literal:port) via the OS resolver and
/// return only the socket addresses that pass the SSRF screen — link-local and
/// unspecified addresses (the genuine metadata/SSRF targets) are dropped.
/// Loopback and private-LAN are intentionally KEPT (local-first platform).
/// An empty result means every resolved address was screened out → the caller
/// must refuse the connection (fail-closed).
pub(crate) fn screened_socket_addrs(target: &str) -> std::io::Result<Vec<SocketAddr>> {
    Ok(target
        .to_socket_addrs()?
        .filter(|sa| !is_link_local_or_unspecified(sa.ip()))
        .collect())
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-agent-runtime reqwest_guard::tests::screened_socket_addrs`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/sandbox/reqwest_guard.rs
git commit -m "feat(egress): reusable screened_socket_addrs (SSRF resolve+screen)"
```

---

### Task 2: Screen + IP-pin the proxy CONNECT

**Files:**
- Modify: `mur-agent-runtime/src/sandbox/egress_proxy.rs`

**Interfaces:**
- Consumes: `screened_socket_addrs` (Task 1); the existing `handle_conn`.

- [ ] **Step 1: Write the failing test**

Add to `egress_proxy.rs`'s test module (it already has `upstream()` + `connect_via(proxy, token, target)` helpers and a `broad_audited_allows_all_except_deny` test to model on):

```rust
#[tokio::test]
async fn broad_audited_link_local_target_is_ssrf_denied() {
    // A broad-audited grant (allow-all-except-deny) must STILL refuse a CONNECT
    // to a link-local / cloud-metadata IP — the SSRF screen backstops the
    // hostname allow/deny list.
    let (proxy, token) = crate::sandbox::egress_proxy::test_broad_proxy().await; // helper below
    let resp = connect_via(proxy, &token, "169.254.169.254:80").await;
    assert!(resp.contains("403"), "link-local CONNECT must be 403, got: {resp}");
}
```

If a `test_broad_proxy()` constructor doesn't already exist, model the setup on the existing `broad_audited_allows_all_except_deny` test (start the proxy, register a broad `PolicyEntry` under a token). Reuse whatever registration path that test uses; the assertion is the point: a link-local target → `403`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-agent-runtime egress_proxy::tests::broad_audited_link_local -- --nocapture`
Expected: FAIL (currently the proxy connects to the link-local IP → not a 403).

- [ ] **Step 3: Implement**

In `handle_conn`, replace the single `let mut upstream = TcpStream::connect(target).await?;` (currently ~line 154, right after the `CONNECT ALLOW` audit) with a screen-and-pin block:

```rust
    // SSRF screen + IP-pin: resolve the CONNECT target once, drop link-local /
    // unspecified (cloud-metadata) addresses, and connect to the pinned
    // SocketAddr (no re-resolution → no DNS-rebinding window). Loopback/LAN are
    // intentionally kept (local-first). This backstops the hostname allow/deny
    // list for browser-rendered sub-resource CONNECTs the gateway never screened.
    let safe_addrs = super::reqwest_guard::screened_socket_addrs(target).unwrap_or_default();
    let Some(pinned) = safe_addrs.into_iter().next() else {
        tracing::info!(host, reason = "ssrf", "egress proxy CONNECT DENY");
        client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await?;
        return Ok(());
    };
    let mut upstream = TcpStream::connect(pinned).await?;
```

(Keep the existing `CONNECT ALLOW` audit line before this — or move it to after `pinned` is chosen so the audit reflects a genuinely-connectable target. Minimal change: leave ALLOW where it is; the DENY-on-ssrf is the new branch.)

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-agent-runtime egress_proxy -- --nocapture`
Expected: PASS (new SSRF-deny test + existing allow/deny tests still green — a normal public/loopback target still tunnels, because `screened_socket_addrs` keeps it).

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/sandbox/egress_proxy.rs
git commit -m "fix(egress): SSRF-screen + IP-pin the proxy CONNECT target"
```

---

### Task 3: Guard test — restricted worker emits no general `*:port` allow

**Files:**
- Modify: `mur-agent-runtime/src/sandbox/macos.rs` (test module)

**Interfaces:**
- Consumes: `build_sbpl_profile(policy)` (existing, `pub`), `SandboxPolicy`.

- [ ] **Step 1: Write the failing/guard test**

The airtight guarantee assumes a restricted worker has NO general `(allow
network-outbound (remote tcp "*:PORT"))` — only loopback carve-outs. Encode that
invariant. Add to `macos.rs`'s test module (model the `SandboxPolicy`
construction on the existing SBPL tests there):

```rust
#[test]
fn restricted_loopback_only_policy_has_no_wildcard_tcp_allow() {
    // A worker whose egress is ONLY the loopback proxy: empty net_allow_ports,
    // one loopback proxy port. (Mirror how build_sbpl_profile is exercised in
    // the sibling tests for constructing a SandboxPolicy.)
    let mut policy = SandboxPolicy::default();
    policy.net_allow_ports = Some(Vec::new());      // deny all general egress
    policy.net_allow_loopback_ports = vec![58999];  // only the loopback proxy
    let sbpl = build_sbpl_profile(&policy);

    assert!(sbpl.contains("(deny network-outbound)"));
    // The loopback carve-out is present…
    assert!(sbpl.contains("(remote tcp \"localhost:58999\")"));
    // …and there is NO wildcard-host tcp allow (the escape hatch).
    assert!(
        !sbpl.contains("(remote tcp \"*:"),
        "restricted loopback-only worker must not emit a wildcard-host tcp allow:\n{sbpl}"
    );
}
```

**Interface note:** confirm `SandboxPolicy`'s field names (`net_allow_ports:
Option<Vec<u16>>`, `net_allow_loopback_ports: Vec<u16>`) and that
`build_sbpl_profile` is `pub` — grep `pub fn build_sbpl_profile` and `pub struct
SandboxPolicy` / `Default for SandboxPolicy`. Adjust the constructor to the real
field names/types; the assertion is the invariant.

- [ ] **Step 2: Run to verify it passes**

Run: `cargo test -p mur-agent-runtime restricted_loopback_only_policy_has_no_wildcard`
Expected: PASS (this codifies existing correct behavior — it's a regression guard so a future change that adds a `*:port` allow to a loopback-only worker fails loudly).

- [ ] **Step 3: Commit**

```bash
git add mur-agent-runtime/src/sandbox/macos.rs
git commit -m "test(egress): guard restricted worker emits no wildcard tcp allow"
```

---

### Task 4: Docs

**Files:**
- Modify: `docs/design/deep-research/README.md` (§5 advisory-enforcement paragraph)
- Modify: `docs/architecture/runtime-overview.md` (egress proxy section, if present)

- [ ] **Step 1:** Update the §5 "Advisory-enforcement honesty" note: the egress
  proxy now **SSRF-screens the CONNECT target IP and pins it** (link-local /
  metadata / unspecified refused with `403`; the pinned connect closes the
  DNS-rebinding window), so browser-tier sub-resource egress can no longer reach
  the cloud-metadata endpoint. Note the deliberate scope: loopback/LAN stay
  reachable (local-first); full private-range blocking remains a separate policy
  option. Keep the honesty that this is now a real (not advisory) screen for the
  metadata/link-local class specifically.

- [ ] **Step 2: Commit**

```bash
git add docs/design/deep-research/README.md docs/architecture/runtime-overview.md
git commit -m "docs(egress): proxy SSRF-screen + IP-pin closes metadata/rebinding gap"
```

---

## Self-Review

**Spec coverage:** spike's fix step 1 (proxy SSRF-screen + IP-pin) → Task 1+2; spike's step 2 (assert no `*:port` escape invariant) → Task 3; docs → Task 4. Spike's step 3 (pin the gateway's OWN tier-1 reqwest fetch) is explicitly OPTIONAL/defense-in-depth in the spike and is now backstopped by the proxy screen → deferred, noted here, not a task.

**Placeholder scan:** The `test_broad_proxy()` helper in Task 2 Step 1 is flagged "reuse the existing `broad_audited_allows_all_except_deny` setup if no constructor exists" — a concrete instruction to model on real existing test code, not a vague placeholder. The `SandboxPolicy` field-name confirmation in Task 3 is a grep-first instruction (the invariant is exact). No code-logic placeholders.

**Type consistency:** `is_link_local_or_unspecified(IpAddr)->bool` (made `pub(crate)`) and `screened_socket_addrs(&str)->io::Result<Vec<SocketAddr>>` (Task 1) are consumed verbatim by `handle_conn` (Task 2). `build_sbpl_profile(&SandboxPolicy)->String` (Task 3) is the existing signature. No drift.

**Key correctness note (carried from the spike refinement):** the screen reuses the runtime's DELIBERATE link-local+unspecified-only policy — it does NOT block loopback/private, because the local-first platform legitimately uses them. This closes the identified metadata SSRF + rebinding without breaking Ollama/LAN. A reviewer expecting "block all private" should read this as an intentional, documented scope decision, not an under-fix.
