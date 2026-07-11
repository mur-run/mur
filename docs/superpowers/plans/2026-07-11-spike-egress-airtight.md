# Spike: Airtight Egress for the Research Gateway (pin-to-proxy)

> **Time-boxed de-risking spike, not an implementation plan.** Deliverable: a
> **Go / No-Go** with evidence, and — critically — the RIGHT framing of what
> "airtight egress" actually requires. No production sandbox change from the
> spike itself.

**Question:** Can we make the research-gateway's tier-2/3 egress *airtight* —
close the documented "advisory-enforcement honesty" gap (spec §5) and the
DNS-rebinding window (fetcher.rs advisory comment) — without a fragile
`sandbox_init`/Landlock change that risks breaking the render tier?

**Time-box:** ½ day (mostly code analysis; the answer is in the existing code).

---

## Finding — the framing was wrong; this is a PROXY fix, not a sandbox fix

The phrase "Phase-3 sbpl pin-to-proxy" implied a scary kernel-sandbox change.
Reading the actual code, the OS-level pinning is **already done** for the
deep-research worker, and the real airtight gap is in the **egress proxy code** —
a contained, unit-testable change, not a sandbox change.

### What's already airtight (OS layer)

`mur-agent-runtime/src/sandbox/macos.rs` restricted-network mode emits
`(deny network-outbound)` + a DNS-socket carve-out + **only** loopback-proxy
carve-outs (`remote tcp "localhost:<proxy_port>"`) for a worker whose
`net_allow_ports` is empty. The deep-research worker is exactly this
(`network.outbound = Restricted`, empty allow-list; egress only via the
per-server proxy token). **Direct egress is already OS-denied** — verified live
during the obscura spike (a sandboxed `obscura`/`lightpanda` *direct* fetch got
`CouldntConnect`/`CouldntResolveHost`; only `--proxy` to the loopback proxy
worked). So a render subprocess **cannot** open a direct socket; it is already
forced through the loopback proxy. The sandbox is not the gap.

> Caveat to verify in implementation: SBPL `remote tcp "*:{port}"` (used when
> `net_allow_ports` is non-empty) allows ANY host on that port — a config that
> put 443 in `net_allow_ports` would be a direct-egress escape. The deep-research
> worker does NOT do this (empty allow-list), but the airtight guarantee assumes
> "no general `*:port` allow." That's a config invariant to assert, not a code
> change.

### What is NOT airtight (the real gap — egress proxy)

`mur-agent-runtime/src/sandbox/egress_proxy.rs`, CONNECT handling:

```rust
let host = target.rsplit_once(':').map(|(h,_)| h).unwrap_or(target);
let allowed = match &entry {
    Some(e) if e.broad => !e.deny.iter().any(|p| host_matches_pattern(host, p)),
    Some(e) => host_allowed(host, &e.allow),
    None => false,
};
// ...
let mut upstream = TcpStream::connect(target).await?;   // ← re-resolves the HOSTNAME
```

Two holes:

1. **No SSRF IP screen.** Authorization is by **hostname string** against
   allow/deny lists only. The proxy never checks the *resolved IP*. Under a
   `broad`-audited grant (what deep-research uses), a CONNECT to
   `169.254.169.254:443` (cloud metadata), `10.0.0.1`, or `127.0.0.1` is
   **allowed** (the host isn't in the deny list) and `TcpStream::connect`
   dials it. The gateway's `screen_url` only screened the **top-level** fetch
   URL — a **browser-rendered page's sub-resource** (`<img src=http://169.254.169.254/…>`,
   redirects, XHR) issues its own CONNECT through the proxy that the gateway
   never screened. The proxy is the only component that sees *all* egress, and
   it does no IP screening → **real SSRF to private ranges / metadata.**
2. **DNS-rebinding window.** `TcpStream::connect(target)` re-resolves the
   hostname at connect time. Even a host the gateway screened as public can
   resolve to a private IP at the proxy's connect → rebinding. No IP pinning.

The runtime already has the screening primitive:
`mur-agent-runtime/src/sandbox/reqwest_guard.rs`
(`is_link_local_or_unspecified` + resolve-and-filter) — currently applied to the
runtime's *own* reqwest client, NOT the proxy's CONNECT target.

## The fix (contained, low-risk, in the proxy)

Before `TcpStream::connect(target)` in `egress_proxy.rs`:
1. Resolve the CONNECT host to its IP(s) **once**.
2. **SSRF-screen** each resolved IP: reject loopback / private (10/8, 172.16/12,
   192.168/16) / link-local (169.254/16, fe80::/10) / unique-local (fc00::/7) /
   unspecified. (Extend `reqwest_guard`'s partial check — it currently covers
   link-local + unspecified — to the full private set, or reuse the gateway's
   fuller `net_guard` logic lifted into a shared helper.)
3. **Connect to the screened `SocketAddr`** (not the hostname string) → pins the
   IP, closing the rebinding window (no second resolution).
4. On a screened-out IP → `403` + audit `CONNECT DENY reason=ssrf`.

This is entirely within `egress_proxy.rs` (+ a screening helper). **No
`sandbox_init`/SBPL/Landlock change.** Unit-testable (feed a CONNECT to a
private IP → expect DENY + no upstream connect; feed a public IP → ALLOW pinned).
The render tier is unaffected (legitimate public targets still connect).

## Go / No-Go

**GO — reframed.** "Airtight egress" is achievable as a **proxy-side SSRF-screen
+ IP-pin**, which is low-risk and testable, NOT the feared fragile sandbox
change (that layer is already airtight for the correctly-configured worker).

Concretely, an implementation plan should:
1. Add IP-level SSRF screening + IP-pinned connect to `egress_proxy.rs` (the
   security fix — closes browser-sub-resource SSRF + rebinding).
2. Assert the config invariant "restricted worker has no general `*:port`
   allow" (a test over the emitted SBPL / policy, not a code change).
3. (Optional, defense-in-depth) pin the gateway's own tier-1 `reqwest` fetch to
   the screened IP (the fetcher.rs advisory TODO) — smaller, since the proxy
   screen now backstops it.

**No-Go trigger (none hit):** would have been "airtight requires host-scoped SBPL
rules" (impossible — SBPL `remote tcp` only accepts `*`/`localhost`) — but that's
moot because the OS layer already denies direct egress; the proxy is the choke
point and the fix lives there.

## Why this matters beyond deep-research

The egress proxy is the single audited choke point for ANY sandboxed MCP server
with a `BroadAudited` grant (not just the research gateway). Adding SSRF
screening at the proxy hardens **every** broad-audited egress consumer against
private-range / metadata SSRF — a general security upgrade, one place.
