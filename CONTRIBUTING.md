# Contributing to mur

## axum middleware in this workspace

**TL;DR:** Do not use axum extractors as parameters in `from_fn` middleware functions in this workspace. Read extensions directly instead. See below for why and how.

### The dual-version situation (issue #39)

The workspace currently carries **two versions of axum** in its dependency graph:

- **axum 0.8** — direct dependency of `mur-core` (server feature) and `mur-agent-runtime`
- **axum 0.7** — transitive dependency pulled in by `qdrant-client` → `tonic 0.12`

```
$ cargo tree --workspace -i axum
axum v0.7.9
└── tonic v0.12.3
    └── qdrant-client v1.17.0
        └── mur-core v2.4.1

axum v0.8.9
├── mur-agent-runtime v0.1.0
└── mur-core v2.4.1
```

The root cause is that `qdrant-client` (as of v1.17.0, the latest release) requires `tonic ^0.12`, which in turn requires axum 0.7. Tonic 0.13+ switched to axum 0.8, but qdrant-client has not yet published a release that adopts tonic 0.13. Until it does, the dual-version situation persists.

Track progress on the fix at **[issue #39](https://github.com/mur-run/mur/issues/39)**.

### Why this bites axum middleware

The natural signature for `from_fn` middleware that inspects the peer address is:

```rust
// DO NOT USE — fails to compile in this workspace
async fn loopback_only(
    addr: Option<axum::extract::ConnectInfo<SocketAddr>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // ...
}
```

This fails because Rust resolves the `FromRequestParts` trait implementation for `ConnectInfo` against **axum 0.7** (the version in scope via tonic's transitive dep), while the `from_fn` wrapper is compiled against **axum 0.8**. The trait bounds don't match and the compiler emits a confusing error that looks like a user error, not a dependency conflict.

### The workaround pattern

Read `ConnectInfo` (or any other extension that would normally be an extractor) directly from `req.extensions()`, bypassing `FromRequestParts` entirely:

```rust
// CORRECT — works regardless of which axum version tonic pulls in
pub(crate) async fn loopback_only(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let is_loopback = req
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|axum::extract::ConnectInfo(addr)| addr.ip().is_loopback())
        .unwrap_or(true);
    // ...
}
```

The canonical live example lives in
`mur-core/src/server_agents/mod.rs` — the `loopback_only` function.

### When the fix lands

Once qdrant-client publishes a release that uses tonic 0.13+ (axum 0.8), bump `qdrant-client` in `mur-core/Cargo.toml`, verify `cargo tree --workspace -i axum` shows only one version, then rewrite `loopback_only` (and any other middleware that used this workaround) back to the natural extractor parameter form.

## Insta snapshot tests

Run snapshot tests:

    cargo test -p mur-agent-runtime --test voice_snapshots

If a snapshot mismatch occurs, review the diff with:

    cargo insta review

CI sets `INSTA_UPDATE=no` so a missing or stale snapshot fails the build.

### Updating snapshots locally
1. `cargo insta review` — interactive review of pending snapshot changes.
2. After accepting, commit the `.snap` files alongside the test changes.

### CI policy
CI sets `INSTA_UPDATE=no`, so any stale or missing snapshot fails the build.

## `MUR_AGENT_FORCE_ECHO=1` for fast dev

The companion subsystem uses an LLM for message generation. To run companion
tests or commands without hitting a real provider, set:

    export MUR_AGENT_FORCE_ECHO=1

This is wired in `mur-agent-runtime/src/supervisor.rs`: when the variable is
set, the supervisor skips provider initialisation and routes all LLM calls
through a deterministic stub-echo runner. The integration tests (M8.*) all
use it. There is no real-LLM nightly smoke script yet; run against a live
provider manually by unsetting the variable.

Note: `MUR_LLM_PROVIDER=stub` is NOT currently wired. Use
`MUR_AGENT_FORCE_ECHO=1` instead.

## Content-pool PR review checklist (companion phase 1.1)

When a PR adds entries to `companion/content/<situation>.<locale>.yaml`, the
reviewer must confirm each entry has:

- [ ] `id` — unique within the file
- [ ] `weight` — float, default 1.0
- [ ] `cooldown_days` — non-negative integer
- [ ] `tags` — list, may be empty
- [ ] `source` — required, names where the prompt seed came from (curated, user-derived, etc.)
- [ ] `reviewed_by` — required, names the human who reviewed
- [ ] `prompt_seed` — required, non-empty

Spec §8.5 R6 / risk mitigation: `source` and `reviewed_by` ensure no unattributed
content lands in the embedded pool.
