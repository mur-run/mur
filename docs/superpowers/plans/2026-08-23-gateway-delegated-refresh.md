# Gateway Delegated Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When an Anthropic request fails with 401 because the stored access token has expired, have the gateway ask Claude Code to refresh its own credential, then retry once — instead of returning the raw 401.

**Architecture:** The gateway never reads or redeems the refresh token. On a 401 it inspects the `expiresAt` already present in the credential blob: a *future* expiry means the token was revoked (a probe cannot help, return immediately), a *past* expiry means the ordinary 8-hourly expiry, so it runs `claude auth status` as a subprocess, re-reads the credential, and retries the upstream request exactly once. A negative cache stops an unrepairable credential from spawning a process every cache TTL.

**Tech Stack:** Rust 2024, axum, reqwest, tokio, serde_json, thiserror.

**Spec:** `docs/superpowers/specs/2026-08-23-oauth-reauth-design.md` (in the `mur` repo) — Half 1.

## Global Constraints

- **Repo:** `~/Projects/mur-model-gateway` (NOT the `mur` workspace). All paths below are relative to it.
- **The gateway must never read, log, or transmit `claudeAiOauth.refreshToken`.** Only `accessToken` (which it already forwards) and `expiresAt` (a timestamp, not a secret).
- **Safe-by-default, mirroring `token_source_codex`:** any new field that can touch the user's real credential or spawn a real binary defaults to disabled in every constructor. Only `main.rs` opts in. A test-side `AppState` that forgets to override must be *structurally* unable to spawn `claude`.
- **One retry only.** A second 401 goes back to the caller unchanged. This rule already governs the Codex path (`src/lib.rs`, the comment above `codex_retry_eligible`); do not weaken it.
- **Codex behaviour is unchanged.** Do not refactor `codex::refreshed_access_token` or `codex_retry_eligible`'s Codex semantics.
- **No hardcoded values** — timeouts and cooldowns are named constants with a comment giving the reason for the number.
- Build/test: `cargo test` from the repo root. Lint gate: `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`.

---

### Task 1: Carry `expiresAt` alongside the access token

The credential blob already contains `claudeAiOauth.expiresAt`, but `parse_oauth_blob` throws it away. Everything downstream needs it, and re-reading the keychain a second time would trigger a second macOS permission prompt — so it must ride along in the same cached read.

**Files:**
- Modify: `src/keychain.rs` — `parse_oauth_blob`, `CachedRead`, `cached`, `read_claude_code_oauth`, `read_credentials_file`
- Test: `src/keychain.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing (first task)
- Produces:
  - `pub struct OauthCredential { pub access_token: String, pub expires_at_ms: Option<i64> }` (derives `Debug, Clone, PartialEq, Eq`)
  - `pub fn read_claude_code_credential() -> Result<Option<OauthCredential>, KeychainError>`
  - `pub fn read_credentials_file_credential(path: &std::path::Path) -> Result<Option<OauthCredential>, KeychainError>`
  - `pub fn read_claude_code_oauth() -> Result<Option<String>, KeychainError>` — **signature unchanged**, now a thin wrapper

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `src/keychain.rs`:

```rust
    #[test]
    fn parse_blob_keeps_expiry() {
        let blob = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-test","refreshToken":"x","expiresAt":1787497765291}}"#;
        let c = parse_oauth_blob(blob).unwrap().unwrap();
        assert_eq!(c.access_token, "sk-ant-oat01-test");
        assert_eq!(c.expires_at_ms, Some(1_787_497_765_291));
    }

    #[test]
    fn parse_blob_without_expiry_is_still_valid() {
        // Older Claude Code writes omitted expiresAt. A missing expiry must
        // not fail the read — it degrades to "unknown", never to an error.
        let blob = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-test"}}"#;
        let c = parse_oauth_blob(blob).unwrap().unwrap();
        assert_eq!(c.access_token, "sk-ant-oat01-test");
        assert_eq!(c.expires_at_ms, None);
    }

    #[test]
    fn parse_blob_ignores_non_integer_expiry() {
        let blob = r#"{"claudeAiOauth":{"accessToken":"t","expiresAt":"soon"}}"#;
        assert_eq!(parse_oauth_blob(blob).unwrap().unwrap().expires_at_ms, None);
    }

    #[test]
    fn oauth_wrapper_still_yields_the_bare_token() {
        // read_claude_code_oauth's contract is unchanged for existing callers.
        let blob = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-test","expiresAt":1}}"#;
        let c = parse_oauth_blob(blob).unwrap().unwrap();
        assert_eq!(c.access_token, "sk-ant-oat01-test");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib keychain::tests -- parse_blob`
Expected: FAIL — `no field 'access_token'` / `expected String, found OauthCredential` (the current `parse_oauth_blob` returns `Option<String>`).

- [ ] **Step 3: Change the payload type**

In `src/keychain.rs`, add the struct next to `KeychainError`:

```rust
/// A Claude Code OAuth credential: the token the gateway forwards, plus the
/// non-secret expiry that shipped with it. The refresh token is deliberately
/// NOT represented here — the gateway never redeems it (see the spec's
/// Rejected section), so it must not be able to leak it either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OauthCredential {
    pub access_token: String,
    /// `claudeAiOauth.expiresAt`, milliseconds since the Unix epoch.
    /// `None` when the blob omits it — treat as "unknown", never as expired.
    pub expires_at_ms: Option<i64>,
}
```

Replace `parse_oauth_blob`:

```rust
/// Extract `claudeAiOauth.{accessToken,expiresAt}` from a Claude Code blob.
fn parse_oauth_blob(raw: &str) -> Result<Option<OauthCredential>, KeychainError> {
    let creds: Value = serde_json::from_str(raw.trim())
        .map_err(|e| KeychainError::Malformed(format!("not JSON: {e}")))?;
    let oauth = creds
        .get("claudeAiOauth")
        .ok_or_else(|| KeychainError::Malformed("missing claudeAiOauth".into()))?;
    let access_token = oauth
        .get("accessToken")
        .and_then(|t| t.as_str())
        .ok_or_else(|| KeychainError::Malformed("missing claudeAiOauth.accessToken".into()))?
        .to_string();
    Ok(Some(OauthCredential {
        access_token,
        expires_at_ms: oauth.get("expiresAt").and_then(Value::as_i64),
    }))
}
```

Widen the cache to hold it, and make `cached` generic:

```rust
type CachedRead = Option<(Instant, Result<Option<OauthCredential>, KeychainError>)>;
static CACHE: Mutex<CachedRead> = Mutex::new(None);
```

```rust
fn cached<T: Clone>(
    cache: &Mutex<Option<(Instant, Result<T, KeychainError>)>>,
    ttl: Duration,
    fetch: impl FnOnce() -> Result<T, KeychainError>,
) -> Result<T, KeychainError> {
    let mut slot = cache.lock().unwrap();
    if let Some((at, res)) = slot.as_ref()
        && at.elapsed() < ttl
    {
        return res.clone();
    }
    let res = fetch();
    *slot = Some((Instant::now(), res.clone()));
    res
}
```

Then the read functions — `read_claude_code_oauth` keeps its old signature so no call site changes:

```rust
/// Read the current Claude Code OAuth credential from the OS keychain.
pub fn read_claude_code_credential() -> Result<Option<OauthCredential>, KeychainError> {
    cached(&CACHE, CACHE_TTL, read_keychain_uncached)
}

/// Back-compat wrapper: just the access token, for callers that forward it.
pub fn read_claude_code_oauth() -> Result<Option<String>, KeychainError> {
    Ok(read_claude_code_credential()?.map(|c| c.access_token))
}

/// Read a credential from a Claude Code credentials JSON file (same blob).
pub fn read_credentials_file_credential(
    path: &std::path::Path,
) -> Result<Option<OauthCredential>, KeychainError> {
    match std::fs::read_to_string(path) {
        Ok(raw) => parse_oauth_blob(&raw),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(KeychainError::Backend(format!("read {}: {e}", path.display()))),
    }
}

/// Back-compat wrapper for the file source.
pub fn read_credentials_file(path: &std::path::Path) -> Result<Option<String>, KeychainError> {
    Ok(read_credentials_file_credential(path)?.map(|c| c.access_token))
}
```

`read_keychain_uncached`'s return type becomes `Result<Option<OauthCredential>, KeychainError>` — its body already ends by calling `parse_oauth_blob`, so only the signature changes.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib keychain`
Expected: PASS, including the pre-existing `cached_serves_within_ttl_and_refetches_after_expiry` and `cached_caches_errors_too`.

- [ ] **Step 5: Verify nothing else broke and commit**

```bash
cargo clippy --all-targets -- -D warnings
cargo test
git add src/keychain.rs
git commit -m "feat(keychain): carry expiresAt alongside the access token

The 401 path needs to tell an expired token from a revoked one, and the
expiry is already in the blob. Widening the cached payload keeps it to a
single keychain read — a second read would mean a second macOS permission
prompt. read_claude_code_oauth keeps its signature so callers are untouched.

The refresh token is deliberately absent from OauthCredential: the gateway
never redeems it, so it should not be able to leak it either."
```

---

### Task 2: A probe policy that is disabled unless production opts in

The probe spawns a real binary and can rewrite the user's Claude Code credential. Following the discipline already established for `token_source_codex`, it must be structurally unavailable to tests: disabled in every constructor, enabled only from `main.rs`.

**Files:**
- Modify: `src/lib.rs` — `AppState` (~line 187), its constructors (~line 250)
- Modify: `src/main.rs` — opt in
- Test: `src/lib.rs` (existing `#[cfg(test)] mod tests`, alongside `token_source_for_picks_per_provider`)

**Interfaces:**
- Consumes: nothing
- Produces:
  - `pub enum AuthProbe { Disabled, Command(std::path::PathBuf) }` (derives `Debug, Clone, PartialEq, Eq`)
  - `pub struct AppState { …, pub auth_probe: AuthProbe }` — defaults to `AuthProbe::Disabled`
  - `pub fn AppState::with_default_auth_probe(self) -> Self`
  - `pub const PROBE_KILL_SWITCH_ENV: &str = "MUR_MODEL_GATEWAY_NO_AUTH_PROBE";`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn auth_probe_is_disabled_in_every_constructor() {
        // Mirrors token_source_codex: a test-side AppState must be unable to
        // spawn the real `claude`, not merely unlikely to. Both constructors
        // are checked — the safety property is that NO path out of this impl
        // block leaves the probe armed, so testing only `new` would let
        // `with_version` regress silently.
        let by_new = AppState::new(
            "https://a.test",
            "https://o.test",
            "https://g.test",
            TokenSource::Disabled,
        )
        .unwrap();
        assert_eq!(by_new.auth_probe, AuthProbe::Disabled, "AppState::new");

        let by_version = AppState::with_version(
            "https://a.test",
            "https://o.test",
            "https://g.test",
            TokenSource::Disabled,
            std::sync::Arc::new(cc_version::VersionCache::default()),
        )
        .unwrap();
        assert_eq!(
            by_version.auth_probe,
            AuthProbe::Disabled,
            "AppState::with_version"
        );
    }

**If `cc_version::VersionCache` has no `Default`,** build it the way the crate's
own tests or `main.rs` build one — the point of the assertion is the
constructor, not how the cache is made.


    #[test]
    fn kill_switch_keeps_the_probe_disabled() {
        // with_default_auth_probe is the only enabling path, and it must
        // honour the opt-out even when `claude` is on PATH.
        temp_env::with_var(PROBE_KILL_SWITCH_ENV, Some("1"), || {
            let s = AppState::new(
                "https://a.test",
                "https://o.test",
                "https://g.test",
                TokenSource::Disabled,
            )
            .unwrap()
            .with_default_auth_probe();
            assert_eq!(s.auth_probe, AuthProbe::Disabled);
        });
    }
```

`temp_env` scopes the env var so the test cannot leak into its neighbours. Add it if absent:

```bash
cargo add --dev temp-env
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib -- auth_probe kill_switch`
Expected: FAIL — `cannot find type 'AuthProbe'`.

Both test names are listed because `kill_switch_keeps_the_probe_disabled`
contains no `auth_probe` substring — filtering on `auth_probe` alone would
silently run one of the two and still report success.

- [ ] **Step 3: Add the policy**

In `src/lib.rs`, beside `TokenSource`:

```rust
/// Opt-out for the delegated-refresh probe. Set to `1` to make the gateway
/// return the upstream 401 unchanged instead of asking Claude Code to refresh.
pub const PROBE_KILL_SWITCH_ENV: &str = "MUR_MODEL_GATEWAY_NO_AUTH_PROBE";

/// How the gateway asks the credential's owner to refresh it.
///
/// `Disabled` by default in every constructor — same discipline as
/// `AppState::token_source_codex`. Enabling spawns a real binary that can
/// rewrite the user's Claude Code credential, so a test-side `AppState` must
/// be structurally unable to reach it rather than merely unlikely to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthProbe {
    Disabled,
    /// Absolute path to the `claude` binary, resolved once at startup. Stored
    /// resolved (not looked up per call) so a later PATH change cannot swap
    /// which binary a long-running gateway executes.
    Command(std::path::PathBuf),
}
```

Add the field to `AppState` and set `auth_probe: AuthProbe::Disabled` in every constructor. Then:

```rust
impl AppState {
    /// Point the auth probe at the `claude` binary on PATH. Call from
    /// `main.rs` only — see the `AuthProbe` doc. A no-op when the kill switch
    /// is set or `claude` is not installed (a transplanted credential is a
    /// supported setup: a working token with no owner CLI).
    pub fn with_default_auth_probe(mut self) -> Self {
        if std::env::var(PROBE_KILL_SWITCH_ENV).is_ok_and(|v| v == "1") {
            return self;
        }
        if let Some(p) = which_claude() {
            self.auth_probe = AuthProbe::Command(p);
        }
        self
    }
}

/// Resolve `claude` on PATH to an absolute path. Deliberately not `which`:
/// one small lookup does not earn a dependency.
fn which_claude() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("claude"))
        .find(|c| c.is_file())
}
```

In `src/main.rs`, chain it where `with_default_codex_source` is already called:

```rust
    let state = AppState::new()?
        .with_default_codex_source()
        .with_default_auth_probe();
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib -- auth_probe kill_switch`
Expected: PASS (2 tests) — confirm the count, not just the exit code.

- [ ] **Step 5: Commit**

```bash
cargo clippy --all-targets -- -D warnings
git add src/lib.rs src/main.rs Cargo.toml
git commit -m "feat(gateway): add a disabled-by-default auth probe policy

The probe spawns a real binary that can rewrite the user's Claude Code
credential, so it follows token_source_codex's discipline: Disabled in
every constructor, enabled only from main.rs. The binary path is resolved
once at startup rather than looked up per call, so a PATH change cannot
swap which binary a long-running gateway executes.

MUR_MODEL_GATEWAY_NO_AUTH_PROBE=1 opts out entirely."
```

---

### Task 3: The probe runner — single-flight, negative cache, expiry comparison

Running the probe is only worth it if it actually moves the credential. This task answers "did it?" and refuses to keep asking when the answer stays no.

**Files:**
- Create: `src/auth_probe.rs`
- Modify: `src/lib.rs` — add `mod auth_probe;`
- Test: `src/auth_probe.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `AuthProbe` (Task 2), `keychain::OauthCredential` and `keychain::CACHE_TTL` (Task 1)
- Produces:
  - `pub async fn refresh_via_owner(probe: &AuthProbe, before_ms: Option<i64>) -> ProbeOutcome`
  - `pub enum ProbeOutcome { Refreshed, NoChange, Skipped }` (derives `Debug, Clone, Copy, PartialEq, Eq`)
  - `pub fn reset_probe_state()` — test-only, clears the cooldown

- [ ] **Step 1: Write the failing tests**

Create `src/auth_probe.rs` with only the tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disabled_probe_is_skipped() {
        reset_probe_state();
        assert_eq!(
            refresh_via_owner_with(&AuthProbe::Disabled, Some(1), || Some(2)).await,
            ProbeOutcome::Skipped
        );
    }

    #[tokio::test]
    async fn a_probe_that_does_not_move_the_expiry_reports_no_change() {
        // /usr/bin/true exits 0 and touches nothing — the shape of a probe
        // that runs fine but repairs nothing.
        reset_probe_state();
        let probe = AuthProbe::Command("/usr/bin/true".into());
        assert_eq!(
            refresh_via_owner_with(&probe, Some(1_000), || Some(1_000)).await,
            ProbeOutcome::NoChange
        );
    }

    #[tokio::test]
    async fn no_change_arms_the_cooldown() {
        // Without this, an unrepairable credential spawns a 325MB process
        // every CACHE_TTL forever.
        reset_probe_state();
        let probe = AuthProbe::Command("/usr/bin/true".into());
        assert_eq!(
            refresh_via_owner_with(&probe, Some(1_000), || Some(1_000)).await,
            ProbeOutcome::NoChange
        );
        assert_eq!(
            refresh_via_owner_with(&probe, Some(1_000), || Some(9_999)).await,
            ProbeOutcome::Skipped,
            "second call within the cooldown must not spawn again — note the              read would report a refresh, so only the cooldown can produce Skipped"
        );
    }

    #[tokio::test]
    async fn a_moved_expiry_reports_refreshed() {
        // The happy path, and the only test that would catch an inverted or
        // dropped comparison. Without it every assertion here is NoChange or
        // Skipped, which a `fn(..) -> NoChange` stub would satisfy.
        reset_probe_state();
        let probe = AuthProbe::Command("/usr/bin/true".into());
        let outcome =
            refresh_via_owner_with(&probe, Some(1_000), || Some(2_000)).await;
        assert_eq!(outcome, ProbeOutcome::Refreshed);
    }

    #[tokio::test]
    async fn an_expiry_that_moves_backwards_is_not_a_refresh() {
        // Strictly-greater, not merely different: a store that rolled back is
        // not a successful refresh.
        reset_probe_state();
        let probe = AuthProbe::Command("/usr/bin/true".into());
        assert_eq!(
            refresh_via_owner_with(&probe, Some(2_000), || Some(1_000)).await,
            ProbeOutcome::NoChange
        );
    }

    #[tokio::test]
    async fn a_refresh_clears_a_previously_armed_cooldown() {
        // Arm the cooldown with a fruitless probe, then prove a real refresh
        // releases it — otherwise one failure would suppress probes for 15
        // minutes even after the credential was repaired.
        reset_probe_state();
        let probe = AuthProbe::Command("/usr/bin/true".into());
        assert_eq!(
            refresh_via_owner_with(&probe, Some(1), || None).await,
            ProbeOutcome::NoChange
        );
        assert!(cooldown_active(), "fruitless probe must arm the cooldown");
        reset_probe_state();
        assert_eq!(
            refresh_via_owner_with(&probe, Some(1), || Some(2)).await,
            ProbeOutcome::Refreshed
        );
        assert!(!cooldown_active(), "a refresh must clear the cooldown");
    }

    #[tokio::test]
    async fn a_missing_binary_reports_no_change_not_a_panic() {
        // A transplanted credential is a supported setup: a valid token with
        // no owner CLI installed.
        reset_probe_state();
        let probe = AuthProbe::Command("/nonexistent/claude".into());
        assert_eq!(
            refresh_via_owner_with(&probe, Some(1), || Some(2)).await,
            ProbeOutcome::NoChange
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib auth_probe::tests`
Expected: FAIL — `cannot find function 'refresh_via_owner'`.

- [ ] **Step 3: Implement the runner**

Prepend to `src/auth_probe.rs`:

```rust
//! Delegated refresh: ask the credential's owner CLI to refresh it, then
//! check whether it did. The gateway never reads or redeems the refresh
//! token itself — see the spec's Rejected section for why.

use crate::AuthProbe;
use crate::keychain;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Long enough for a 325MB binary to cold-start and answer, short enough that
/// a wedged probe cannot hold a request open. `claude auth status` answers in
/// well under a second warm.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// After a probe that changed nothing, wait this long before spawning again.
/// Well above `keychain::CACHE_TTL` (60s) — without that gap an unrepairable
/// credential would spawn a process on every cache miss, indefinitely.
const PROBE_COOLDOWN: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The owner refreshed the credential; the caller should re-read and retry.
    Refreshed,
    /// The probe ran (or could not run) and the credential did not move.
    NoChange,
    /// No probe was attempted: disabled, or inside the cooldown.
    Skipped,
}

/// Serialises probes: held across the child's execution so concurrent 401s
/// queue behind one probe rather than each spawning their own — the same
/// single-flight shape as `codex::refreshed_access_token`. `tokio::sync::Mutex`
/// is deliberate: a waiter yields instead of parking a worker thread, and there
/// is no poisoning to propagate.
static PROBE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// When the last fruitless probe ran. Deliberately a `std::sync::Mutex`, NOT
/// part of `PROBE_LOCK`: `reset_probe_state` is a sync fn called from
/// `#[tokio::test]` bodies, and `blocking_lock()` on a tokio mutex panics
/// inside a runtime. Never hold this across an await — take it, read or write,
/// drop it.
static COOLDOWN: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

fn probe_lock() -> &'static Mutex<()> {
    PROBE_LOCK.get_or_init(|| Mutex::new(()))
}

fn cooldown_active() -> bool {
    COOLDOWN
        .lock()
        .unwrap()
        .is_some_and(|at| at.elapsed() < PROBE_COOLDOWN)
}

/// Ask the owner CLI to refresh, then report whether the stored expiry moved.
///
/// `before_ms` is the `expiresAt` observed before the probe; the credential is
/// re-read afterwards and the two compared. A blob without an expiry compares
/// as unchanged, which is the safe direction: it costs one wasted retry, not a
/// spawn loop.
pub async fn refresh_via_owner(probe: &AuthProbe, before_ms: Option<i64>) -> ProbeOutcome {
    let AuthProbe::Command(bin) = probe else {
        return ProbeOutcome::Skipped;
    };

    let mut last_fruitless = lock().lock().await;
    if let Some(at) = *last_fruitless
        && at.elapsed() < PROBE_COOLDOWN
    {
        return ProbeOutcome::Skipped;
    }

    // stdin is closed, not inherited: a probe that decides to prompt must fail
    // fast rather than wait forever on a daemon's stdin.
    let child = tokio::process::Command::new(bin)
        .args(["auth", "status"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();

    match child {
        Ok(mut c) => {
            if tokio::time::timeout(PROBE_TIMEOUT, c.wait()).await.is_err() {
                tracing::warn!("auth probe timed out after {PROBE_TIMEOUT:?}");
                let _ = c.kill().await;
            }
        }
        Err(e) => {
            // Missing binary (transplanted credential) or no permission.
            tracing::warn!(error = %e, bin = %bin.display(), "auth probe could not start");
            *last_fruitless = Some(Instant::now());
            return ProbeOutcome::NoChange;
        }
    }

    let after_ms = keychain::read_claude_code_credential()
        .ok()
        .flatten()
        .and_then(|c| c.expires_at_ms);

    match (before_ms, after_ms) {
        (Some(before), Some(after)) if after > before => {
            *last_fruitless = None;
            ProbeOutcome::Refreshed
        }
        _ => {
            tracing::warn!(
                "auth probe did not refresh the credential; \
                 backing off for {PROBE_COOLDOWN:?}"
            );
            *last_fruitless = Some(Instant::now());
            ProbeOutcome::NoChange
        }
    }
}

/// Clear the cooldown. Test-only: the state is process-global and would
/// otherwise leak between tests in the same binary. Sync, and touches only
/// `COOLDOWN`, so it is safe to call from inside a `#[tokio::test]`.
pub fn reset_probe_state() {
    *COOLDOWN.lock().unwrap() = None;
}
```

**The credential re-read is injectable.** `refresh_via_owner` is the feature's
core decision — it decides whether the gateway retries and whether it arms a
15-minute cooldown — and every outcome except one depends on what the re-read
returns. Reading the machine's real keychain directly would leave the
`Refreshed` path with no test at all. So split it:

```rust
/// Ask the owner CLI to refresh, then report whether the stored expiry moved.
pub async fn refresh_via_owner(probe: &AuthProbe, before_ms: Option<i64>) -> ProbeOutcome {
    refresh_via_owner_with(probe, before_ms, || {
        // Bypass the 60s memoise: the child just rewrote the store, and a
        // cached read would return the token we already know is dead.
        keychain::invalidate_cache();
        keychain::read_claude_code_credential()
            .ok()
            .flatten()
            .and_then(|c| c.expires_at_ms)
    })
    .await
}

/// The body, with the post-probe credential read injected. Tests drive every
/// outcome through this; production goes through the wrapper above.
pub(crate) async fn refresh_via_owner_with(
    probe: &AuthProbe,
    before_ms: Option<i64>,
    read_after: impl FnOnce() -> Option<i64>,
) -> ProbeOutcome {
    // …body as below, calling `read_after()` where the re-read happens…
}
```

Note: the re-read must bypass the 60s cache to see a fresh write. `read_claude_code_credential` is cached, so add a cache-busting reset in `src/keychain.rs`:

```rust
/// Drop the memoised read so the next call hits the store. Used after an
/// external process is believed to have rewritten the credential.
pub fn invalidate_cache() {
    *CACHE.lock().unwrap() = None;
}
```

That reset is called from the `refresh_via_owner` wrapper shown above, not from the injectable body — tests must not touch the real keychain.

Register the module in `src/lib.rs`:

```rust
pub mod auth_probe;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib auth_probe`
Expected: PASS (4 tests).

`reset_probe_state` uses `blocking_lock`, which panics inside a runtime. If the tests trip on that, change it to hold no lock at all by storing the cooldown in a `std::sync::Mutex<Option<Instant>>` separate from the tokio single-flight mutex, and reset only that.

- [ ] **Step 5: Commit**

```bash
cargo clippy --all-targets -- -D warnings
git add src/auth_probe.rs src/lib.rs src/keychain.rs
git commit -m "feat(gateway): delegated-refresh probe with single-flight and cooldown

Asks the owner CLI to refresh, then compares the stored expiry to decide
whether it worked. A probe that changes nothing arms a 15-minute cooldown:
without it an unrepairable credential would spawn a 325MB process on every
cache miss, forever.

stdin is closed rather than inherited so a probe that decides to prompt
fails fast instead of hanging a daemon."
```

---

### Task 4: Wire the probe into the 401 path

**Files:**
- Modify: `src/lib.rs` — the retry block (~line 579) and `codex_retry_eligible` (~line 764)
- Test: `tests/auth_probe_retry.rs` (create)

**Interfaces:**
- Consumes: `auth_probe::{refresh_via_owner, ProbeOutcome}` (Task 3), `keychain::read_claude_code_credential` (Task 1), `AppState::auth_probe` (Task 2)
- Produces: `fn anthropic_retry_eligible(provider: Provider, status: reqwest::StatusCode, expires_at_ms: Option<i64>, now_ms: i64) -> bool`

- [ ] **Step 1: Write the failing test**

Create `tests/auth_probe_retry.rs`:

```rust
//! The expired-vs-revoked branch. A 401 whose stored expiry is still in the
//! future means the token was revoked upstream, and no refresh can help — so
//! the gateway must not spawn anything.

use mur_model_gateway::{Provider, TokenSource, anthropic_retry_eligible};
use reqwest::StatusCode;
use std::sync::Arc;

const NOW_MS: i64 = 1_787_000_000_000;

#[test]
fn expired_token_is_eligible() {
    assert!(anthropic_retry_eligible(
        Provider::Anthropic,
        StatusCode::UNAUTHORIZED,
        &TokenSource::Keychain,
        Some(NOW_MS - 1),
        NOW_MS
    ));
}

#[test]
fn a_source_claude_code_does_not_own_is_never_eligible() {
    // A raw key from the environment, or a test's Static token, is not
    // something `claude auth status` can refresh — asking it to would spawn a
    // process that cannot possibly help. Same principle as the Codex arm's
    // ApiKey case: a 401 on a key means the key is rejected, and resending it
    // cannot succeed.
    for src in [
        TokenSource::EnvVar("ANTHROPIC_API_KEY".into()),
        TokenSource::Static(Arc::new("sk-ant-raw".to_string())),
        TokenSource::Disabled,
    ] {
        assert!(
            !anthropic_retry_eligible(
                Provider::Anthropic,
                StatusCode::UNAUTHORIZED,
                &src,
                Some(NOW_MS - 1),
                NOW_MS
            ),
            "{src:?} must not be eligible"
        );
    }
}

#[test]
fn a_credentials_file_source_is_eligible() {
    // The Linux and Windows install shape: Claude Code owns the file, so a
    // delegated refresh is exactly as applicable as it is for the keychain.
    assert!(anthropic_retry_eligible(
        Provider::Anthropic,
        StatusCode::UNAUTHORIZED,
        &TokenSource::CredentialsFile("/tmp/creds.json".into()),
        Some(NOW_MS - 1),
        NOW_MS
    ));
}

#[test]
fn revoked_token_is_not_eligible() {
    // 401 while the stored expiry is still in the future: revoked, not expired.
    assert!(!anthropic_retry_eligible(
        Provider::Anthropic,
        StatusCode::UNAUTHORIZED,
        &TokenSource::Keychain,
        Some(NOW_MS + 60_000),
        NOW_MS
    ));
}

#[test]
fn unknown_expiry_is_eligible_once() {
    // Older blobs carry no expiresAt. Allow the probe; the cooldown bounds
    // the cost if it turns out to be fruitless.
    assert!(anthropic_retry_eligible(
        Provider::Anthropic,
        StatusCode::UNAUTHORIZED,
        &TokenSource::Keychain,
        None,
        NOW_MS
    ));
}

#[test]
fn non_401_is_never_eligible() {
    assert!(!anthropic_retry_eligible(
        Provider::Anthropic,
        StatusCode::INTERNAL_SERVER_ERROR,
        &TokenSource::Keychain,
        Some(NOW_MS - 1),
        NOW_MS
    ));
}

#[test]
fn other_providers_are_never_eligible() {
    // Codex keeps its own path; OpenAI and Gemini have no delegated owner.
    for p in [Provider::OpenAI, Provider::Gemini, Provider::Codex] {
        assert!(!anthropic_retry_eligible(
            p,
            StatusCode::UNAUTHORIZED,
            &TokenSource::Keychain,
            Some(NOW_MS - 1),
            NOW_MS
        ));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test auth_probe_retry`
Expected: FAIL — `cannot find function 'anthropic_retry_eligible'`.

- [ ] **Step 3: Add the predicate and the retry arm**

Beside `codex_retry_eligible` in `src/lib.rs`:

```rust
/// Whether an Anthropic 401 is worth a delegated refresh.
///
/// A 401 with the stored expiry still in the future means the credential was
/// revoked upstream, not that it aged out — a refresh cannot fix that, so the
/// gateway must not spawn a probe for it. A blob with no expiry is allowed
/// through once; the probe's own cooldown bounds the cost if it is fruitless.
pub fn anthropic_retry_eligible(
    provider: Provider,
    status: reqwest::StatusCode,
    source: &TokenSource,
    expires_at_ms: Option<i64>,
    now_ms: i64,
) -> bool {
    // Only a store Claude Code owns can be repaired by asking Claude Code to
    // refresh. A raw key from the environment is rejected on its own merits.
    let claude_owned = matches!(
        source,
        TokenSource::Keychain | TokenSource::CredentialsFile(_)
    );
    provider == Provider::Anthropic
        && status == reqwest::StatusCode::UNAUTHORIZED
        && claude_owned
        && expires_at_ms.is_none_or(|exp| exp <= now_ms)
}

/// The stored expiry for Anthropic, read from **the same source the token came
/// from**. Reading the keychain unconditionally would be wrong on every Linux
/// and Windows install, where Claude Code writes
/// `~/.claude/.credentials.json` and there is no keychain to read — the expiry
/// would come back unknown on every request and the probe would fire on every
/// 401.
fn anthropic_credential_expiry(state: &AppState) -> Option<i64> {
    let cred = match state.token_source_for(Provider::Anthropic) {
        TokenSource::Keychain => keychain::read_claude_code_credential().ok().flatten(),
        TokenSource::CredentialsFile(p) => {
            keychain::read_credentials_file_credential(p).ok().flatten()
        }
        _ => None,
    };
    cred.and_then(|c| c.expires_at_ms)
}
```

Then add a second retry arm after the existing Codex one (~line 579). It mirrors that block's header handling — forward the same client headers as the first attempt, minus hop-by-hop/host/content-length and the auth header being replaced:

```rust
    // Anthropic: the owner CLI holds the refresh token, so ask it to refresh
    // rather than redeeming the token here. One retry only, same as Codex.
    let anthropic_expiry = anthropic_credential_expiry(state);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    if anthropic_retry_eligible(
        provider,
        upstream_resp.status(),
        state.token_source_for(Provider::Anthropic),
        anthropic_expiry,
        now_ms,
    )
        && auth_probe::refresh_via_owner(&state.auth_probe, anthropic_expiry).await
            == auth_probe::ProbeOutcome::Refreshed
    {
        // …rebuild the request exactly as the Codex arm does, with the token
        // from a fresh read of the SAME source (`anthropic_credential_expiry`'s
        // match arm shows which), send once,
        // and use that response.
    }
```

Read the Codex arm immediately above and mirror its header-forwarding verbatim; the comments there record two separate production bugs (dropped content-type/accept, and a re-read that lost the account header) that this arm must not reintroduce.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test auth_probe_retry && cargo test`
Expected: PASS, and every pre-existing test still green — especially `tests/codex.rs`, which pins the Codex arm's behaviour.

- [ ] **Step 4b: Prove the whole arm works, not just the predicate**

The predicate tests above cover the decision. Nothing yet covers the *arm* —
401 → probe → re-read → retry → 200 — which is the entire point of the task.
`TokenSource::CredentialsFile` makes that testable without touching a real
keychain or a real `claude`.

Create `tests/anthropic_retry_arm.rs`:

```rust
//! The delegated-refresh arm end to end: an expired credential in a file, a
//! fake `claude` that rewrites it, and an upstream that 401s once then serves.

use std::io::Write;

fn blob(expires_at_ms: i64) -> String {
    format!(r#"{{"claudeAiOauth":{{"accessToken":"sk-ant-oat01-x","expiresAt":{expires_at_ms}}}}}"#)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_expired_credential_is_refreshed_and_the_request_retried() {
    let dir = tempfile::tempdir().unwrap();
    let creds = dir.path().join("creds.json");
    let past = 1_000_i64;
    let future = 9_999_999_999_999_i64;
    std::fs::write(&creds, blob(past)).unwrap();

    // A fake `claude` that does what the real one does for our purposes:
    // rewrite the credential with a later expiry.
    let fake = dir.path().join("claude");
    let mut f = std::fs::File::create(&fake).unwrap();
    writeln!(
        f,
        "#!/bin/sh
cat > '{}' <<'EOF'
{}
EOF",
        creds.display(),
        blob(future)
    )
    .unwrap();
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let server = httpmock::MockServer::start_async().await;
    // Assert on call counts at the end: the first attempt must 401 and the
    // retry must succeed, so exactly two requests reach the upstream.
    let m = server
        .mock_async(|when, then| {
            when.any_request();
            then.status(401).body("expired");
        })
        .await;

    // …build an AppState pointed at `server.base_url()` with
    // `TokenSource::CredentialsFile(creds.clone())` and
    // `auth_probe: AuthProbe::Command(fake)`, drive one request through
    // `proxy`, and assert:
    //   * the upstream saw 2 requests (m.hits_async().await == 2)
    //   * the credential file now holds `future`, i.e. the probe ran
    //   * the second attempt carried the refreshed token
    //
    // Follow `tests/codex.rs` for how this crate stands up an AppState against
    // httpmock and issues a request — mirror its setup rather than inventing
    // one, and reuse whatever seam it uses to override upstreams.
    let _ = (m, past);
}
```

Read `tests/codex.rs` first and mirror its harness. If the crate has no seam
that lets a test drive `proxy` directly, say so in your report and cover as
much of the arm as the existing seams allow rather than adding new public
surface to make the test possible.

- [ ] **Step 5: Commit**

```bash
cargo clippy --all-targets -- -D warnings
cargo fmt
git add -A
git commit -m "feat(gateway): delegated refresh on an expired Anthropic 401

Closes the asymmetry: a Codex 401 refreshed and retried, an Anthropic 401
went straight back to the caller. The Anthropic arm never touches the
refresh token — it asks Claude Code to refresh, re-reads, and retries once.

A 401 whose stored expiry is still in the future means the credential was
revoked rather than expired, so no probe is attempted for it."
```

---

### Task 5: Say what expired and where

**Files:**
- Modify: `src/lib.rs` — the response path after a failed retry
- Test: `tests/auth_probe_retry.rs` (extend)

**Interfaces:**
- Consumes: everything above
- Produces: `pub fn anthropic_auth_error_body(expired: bool) -> String`

- [ ] **Step 1: Write the failing test**

Append to `tests/auth_probe_retry.rs`:

```rust
#[test]
fn error_body_names_the_fix() {
    let b = mur_model_gateway::anthropic_auth_error_body(&TokenSource::Keychain, true);
    assert!(b.contains("/login anthropic"), "names the fix: {b}");
    assert!(b.contains("claude auth login"), "names the CLI fallback: {b}");
}

#[test]
fn error_body_names_the_store_the_token_came_from() {
    // A file-backed install must not be told to look in a keychain it does not
    // have. This is the fourth place in this plan where hardcoding the keychain
    // would have been wrong.
    let b = mur_model_gateway::anthropic_auth_error_body(
        &TokenSource::CredentialsFile("/home/u/.claude/.credentials.json".into()),
        true,
    );
    assert!(b.contains("/home/u/.claude/.credentials.json"), "{b}");
    assert!(
        !b.contains("Claude Code-credentials"),
        "must not name the keychain for a file source: {b}"
    );
}

#[test]
fn revoked_body_does_not_promise_a_refresh() {
    // Re-running a refresh cannot fix a revoked credential; saying so would
    // send the user in circles.
    let b = mur_model_gateway::anthropic_auth_error_body(&TokenSource::Keychain, false);
    assert!(b.contains("revoked"), "{b}");
    assert!(!b.contains("expired"), "{b}");
}

#[test]
fn error_body_never_contains_the_token() {
    // describe_credential_store falls through to `{other:?}` for the remaining
    // variants, and TokenSource::Static holds a real token. The redacting Debug
    // added in Task 4 is what keeps this true — this test is its guard from the
    // other side.
    let b = mur_model_gateway::anthropic_auth_error_body(
        &TokenSource::Static(std::sync::Arc::new("sk-ant-secret-value".to_string())),
        true,
    );
    assert!(!b.contains("sk-ant-secret-value"), "token leaked into an error body: {b}");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test auth_probe_retry error_body`
Expected: FAIL — `cannot find function 'anthropic_auth_error_body'`.

- [ ] **Step 3: Implement**

```rust
/// A 401 the caller can act on. The upstream body names no location, which is
/// what left users guessing which credential had gone stale.
///
/// The store is derived from the source the token actually came from — NOT a
/// hardcoded keychain string. On a Linux or Windows install the credential
/// lives in a file, and an error that confidently names the wrong store sends
/// the reader to a place that does not exist. This is the same mistake the
/// expiry read made in three separate places earlier in this plan; do not
/// reintroduce it here.
pub fn anthropic_auth_error_body(source: &TokenSource, expired: bool) -> String {
    let what = if expired {
        "Anthropic OAuth expired and an automatic refresh did not resolve it"
    } else {
        "Anthropic OAuth was revoked (the stored credential has not aged out)"
    };
    format!(
        "{what} — credential: {}. \
         Fix: run `/login anthropic` in murmur, or `claude auth login`.",
        describe_credential_store(source)
    )
}

/// Where a reader should go to look at the credential, in words. Never prints
/// the credential itself — `TokenSource`'s own `Debug` redacts, and this must
/// not become a way around that.
fn describe_credential_store(source: &TokenSource) -> String {
    match source {
        TokenSource::Keychain => {
            if cfg!(target_os = "macos") {
                "keychain \"Claude Code-credentials\"".to_string()
            } else {
                // The non-macOS fallback `resolve_credential` uses.
                format!(
                    "{}",
                    keychain::default_credentials_path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "~/.claude/.credentials.json".to_string())
                )
            }
        }
        TokenSource::CredentialsFile(p) => p.display().to_string(),
        other => format!("{other:?}"),
    }
}
```

Return it in place of the raw upstream body when the Anthropic 401 was not repaired. Preserve the 401 status code — only the body changes.

**Then prove it reaches the client.** The pure-function tests above cover the
wording; nothing yet covers the wiring, and every task in this plan that tested
only its predicate turned out to have an untested arm. `tests/anthropic_retry_arm.rs`
is already ungated and already stands up an `AppState` against httpmock, so add
a case there rather than building a new harness: an eligible 401 whose probe
does **not** repair the credential (a fake `claude` that changes nothing) must
return 401 to the client with a body containing `/login anthropic` — not the
upstream's original body. Assert the upstream saw exactly two hits, so the case
is genuinely the post-retry path and not an early return.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit and verify the whole gate**

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
git add -A
git commit -m "feat(gateway): actionable body for an unrepaired Anthropic 401

The upstream body says 're-authenticate' and names neither the credential
nor where it lives. Expired and revoked get different wording because
re-running a refresh cannot fix a revoked credential."
```

---

## Manual verification

Automated tests cannot cover the real spawn. After Task 5:

1. `MUR_MODEL_GATEWAY_NO_AUTH_PROBE=1 mur-model-gateway` — confirm an expired-token 401 returns the actionable body and `ps` shows no `claude` child.
2. Without the kill switch, with a genuinely expired token (wait past `expiresAt`, or point `TokenSource::CredentialsFile` at a copied blob whose `expiresAt` is in the past): confirm one `claude` child is spawned, the request succeeds on retry, and `security find-generic-password -s "Claude Code-credentials"` shows a newer `mdat`.
3. Repeat step 2 with a credential that cannot be repaired: confirm exactly one spawn, then silence for 15 minutes.

**Step 2 answers the spec's first open question** — whether `claude auth status` refreshes an expired token. Record the result in the spec.
