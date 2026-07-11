# Spike: Loopback-Only Pin-to-Proxy (force ALL worker egress through the proxy)

> **Time-boxed de-risking spike, not an implementation plan.** Deliverable: a
> **Go / No-Go** with evidence + the concrete (contained) change path. Follow-up
> to #689 (proxy SSRF-screen + IP-pin), which hardened *proxied* traffic but did
> NOT force traffic through the proxy.

**Question:** Can we make a deep-research worker's SBPL allow **only** the
loopback proxy ports — no `(remote tcp "*:PORT")` — so a compromised render
subprocess CANNOT direct-connect and bypass the egress proxy, without breaking
the worker's legitimate egress?

**Time-box:** ½ day (code analysis + one live-test to validate).

---

## Finding — feasible, because the worker's egress is ALREADY all-loopback

The enabling fact (verified in `mur-core/src/cmd/deep_research/provision.rs`):
- **LLM egress is loopback.** `WORKER_LLM_ALLOW_HOSTS = ["localhost", "127.0.0.1"]`
  — the worker reaches its LLM via the **local cc-proxy** (127.0.0.1), not a
  direct cloud endpoint.
- **Web egress is loopback.** Research fetches go through the **egress proxy**
  (127.0.0.1:`<ephemeral>`) via the per-server `HTTP_PROXY` token.

So a deep-research worker **never legitimately needs direct `*:443`**. The
`net_allow_ports = Some([80,443,8080,8443])` it currently gets is the *generic*
`NetworkOutboundMode::Restricted` default (`policy.rs from_entitlements`) — an
**unused grant** that is precisely the direct-egress escape hatch #689 couldn't
close. Removing it does not remove any capability the worker uses.

## The three blockers (all contained, no new subsystem)

1. **`SandboxPolicy::allow_loopback_ports` Off-mode guard** (`policy.rs`) only
   adds loopback ports when `net_allow_ports` is `Some(non-empty)`:
   ```rust
   if let Some(ports) = &self.net_allow_ports && !ports.is_empty() { … }
   ```
   To be loopback-only we need `net_allow_ports = Some([])` (deny general TCP)
   WITH loopback ports — the guard blocks exactly this. **Fix:** relax the guard
   to also permit adding loopback ports when `net_allow_ports == Some([])` (an
   empty-but-present list = "deny general, allow named loopback"), or add an
   explicit `ProxyOnly` posture. Do NOT allow loopback ports under `None`
   (unrestricted) — meaningless there.

2. **`macos.rs` empty-ports branch** emits only `(deny network-outbound)`:
   ```rust
   Some(ports) if ports.is_empty() => { lines.push("(deny network-outbound)"); }
   ```
   It drops the loopback carve-outs (and mDNS). **Fix:** in this branch, when
   `net_allow_loopback_ports` is non-empty, also emit the
   `(allow network-outbound (remote tcp "localhost:{port}"))` carve-outs — the
   same lines the `Some(non-empty)` branch already emits — but **no** `*:{port}`
   lines. (Landlock/`linux.rs` has the analogous port-gating — mirror there.)

3. **The deep-research worker's net policy** must become
   `net_allow_ports = Some([])` + `net_allow_loopback_ports = [cc_proxy_port,
   egress_proxy_port]` instead of the generic `Restricted` `Some([80,443,…])`.
   This is a provision-side change (a `ProxyOnly`/loopback-only posture for the
   worker), NOT a change to generic `Restricted` mode (other agents still get
   the direct-egress default).

## mDNS nuance (implementation detail, not a blocker)

The worker connects to `localhost`/`127.0.0.1`. An **IP literal** (`127.0.0.1`)
needs no name resolution; the string `"localhost"` may hit `mDNSResponder`
(macOS getaddrinfo). Two clean options: (a) also emit the mDNS unix-socket
carve-out in the loopback-only branch (as the `Some(non-empty)` branch does), or
(b) ensure the worker's proxy targets are 127.0.0.1 IP literals (no resolution).
Prefer (a) for robustness — it's one line and matches the existing branch.

## Go / No-Go

**GO — feasible and contained.** The worker's egress is already 100% loopback, so
removing the unused `*:443` and pinning to loopback-only forces every egress
through the (now SSRF-screening, IP-pinning) proxy — making the #689 proxy the
**airtight single choke point** it was meant to be. Three small, localized
changes (guard relax + empty-branch loopback emit + worker `ProxyOnly` posture),
no new subsystem, no change to generic `Restricted` mode.

**Mandatory validation (the real de-risk — must pass before merge):** provision a
loopback-only worker, run one research turn, and confirm **all three**:
1. LLM turn completes (cc-proxy loopback reachable) — worker can reason.
2. A web `fetch`/`search` succeeds through the egress proxy (loopback reachable).
3. A **direct** `fetch` to `example.com:443` from inside the sandbox is now
   **denied** (`Operation not permitted` / connect refused) — the `*:443` escape
   is gone. (This is the airtight proof; compare against today, where a direct
   443 connect would succeed.)

**No-Go trigger (watch for):** if the worker turns out to need a non-loopback
direct connection we didn't account for (some dependency resolving/ connecting
directly), (3) would break the worker. The live test surfaces this before merge;
if it fails, keep the generic `Restricted` default for that path and scope
loopback-only narrowly.

## Scope note

This is the deep-research/`ProxyOnly` worker's posture. It does NOT change generic
`Restricted` egress (agents that legitimately need direct `*:443` keep it). The
airtight guarantee is: "a worker whose entire egress is proxy-mediated gets an
OS profile that *enforces* it."
