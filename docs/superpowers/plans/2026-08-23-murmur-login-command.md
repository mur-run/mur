# murmur `/login` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give murmur a `/login` command that shows OAuth health for every provider and repairs it in place — using the cheapest rung that works, and handing the terminal to the owner CLI only when a real browser login is unavoidable.

**Architecture:** murmur never reads a secret. It answers "is this healthy?" by asking the owner CLI (`claude auth status --json`, `codex login status`) and "did a refresh just happen?" by comparing credential-store *metadata* (the keychain item's `mdat`, a file's mtime). Repair escalates: re-check → cheap probe → full login. Only the last rung suspends the TUI, and it is the rare one.

**Tech Stack:** Rust 2024, ratatui + crossterm (bottom-anchored `Viewport::Inline`, main screen), tokio, serde_json, chrono.

**Spec:** `docs/superpowers/specs/2026-08-23-oauth-reauth-design.md` — Half 2.

## Global Constraints

- **murmur must never read, log, or store a token.** Permitted sources: the owner CLI's own status output, and credential-store metadata. `security find-generic-password` must never be invoked with `-w`. A test that asserts this is part of Task 2.
- **New code goes in new modules.** `cli/mod.rs` is 2864 lines and `cli/app.rs` is 2572 — both already past the 800-line rule in `CLAUDE.md`. Follow the sibling pattern (`model_cmd.rs`, `memory_cmds.rs`): `cli/login.rs` and `cli/handover.rs`. Only the minimum goes into the existing files.
- **The brand is uppercase `MUR`** in anything a user reads (`CLAUDE.md` rule 7). Provider labels are `Anthropic` and `ChatGPT`.
- **`mur auth login` is a different thing** (mur.run catalog account). Help text must not blur them.
- **No hardcoded values** — timeouts and paths are named constants or derived from `home`.
- Tests: `cargo nextest run -p mur-core <filter>`. Plain `cargo test` reports false failures in this workspace. If a `mur-core` binary target aborts with a stack overflow, re-run with `RUST_MIN_STACK=33554432`.
- Lint gate: `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check`. Clippy must include `--all-targets`, or `tests/` breakage lands unseen.

## File structure

| File | Responsibility |
|---|---|
| `cli/login.rs` (new) | Provider identity, store metadata, owner-CLI status, status rendering, escalating repair, headless detection. No terminal access. |
| `cli/handover.rs` (new) | Suspend the TUI, run an interactive child, restore. Terminal only; knows nothing about OAuth. |
| `cli/app.rs` | `SlashCmd::Login`, its parse arm, the `pending_handover` field. |
| `cli/mod.rs` | `mod` declarations, the dispatch arm, `HELP`, the main-loop handover hook. |

The split is deliberate: `login.rs` is pure enough to unit-test without a terminal, and `handover.rs` is reusable by any future command that needs to hand over.

---

### Task 1: `/login` parses

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/app.rs` — `SlashCmd` (~line 158), `parse_slash` (~line 196)
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` — `HELP` (line 165)
- Test: `mur-core/src/cmd/agent/cli/app.rs` (existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing
- Produces: `SlashCmd::Login(Option<String>)` — the raw provider word, unvalidated at parse time so an unknown provider can be reported with its spelling intact.

- [ ] **Step 1: Write the failing test**

Add beside `parse_slash_model` in `app.rs`:

```rust
    #[test]
    fn parse_slash_login() {
        assert_eq!(parse_slash("/login"), Some(SlashCmd::Login(None)));
        assert_eq!(
            parse_slash("/login anthropic"),
            Some(SlashCmd::Login(Some("anthropic".into())))
        );
        assert_eq!(
            parse_slash("/login chatgpt"),
            Some(SlashCmd::Login(Some("chatgpt".into())))
        );
        // The word is kept verbatim: an unknown provider is reported with the
        // spelling the user typed, not silently dropped.
        assert_eq!(
            parse_slash("/login bogus"),
            Some(SlashCmd::Login(Some("bogus".into())))
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p mur-core parse_slash_login`
Expected: FAIL — `no variant named 'Login'`.

- [ ] **Step 3: Add the variant, the parse arm, and the help entry**

In `app.rs`, add to `SlashCmd` beside `Model`:

```rust
    /// `/login [anthropic|chatgpt]` — show OAuth health, or repair one provider.
    /// Unrelated to `mur auth login`, which signs in to mur.run.
    Login(Option<String>),
```

In `parse_slash`, beside the `"model"` arm:

```rust
        "login" => SlashCmd::Login(words.next().map(str::to_string)),
```

In `mod.rs`, extend `HELP` — insert after the `/model` entry:

```
  /login [anthropic|chatgpt] (OAuth health / re-authenticate)
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo nextest run -p mur-core parse_slash`
Expected: PASS — the new test and every pre-existing `parse_slash_*` test.

The compiler will now reject the `match` in `handle_slash` as non-exhaustive. Add a placeholder arm so the tree builds; Task 4 replaces it:

```rust
        SlashCmd::Login(_) => app.push_system("not implemented yet"),
```

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add mur-core/src/cmd/agent/cli/app.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(murmur): parse /login [provider]

The provider word is kept verbatim rather than resolved at parse time, so
an unknown provider can be reported back with the spelling the user typed."
```

---

### Task 2: Credential-store metadata, without reading the credential

"Did a refresh just happen?" is answerable from the store's modification time alone. That keeps murmur out of the business of holding tokens.

**Files:**
- Create: `mur-core/src/cmd/agent/cli/login.rs`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` — add `mod login;`
- Test: `mur-core/src/cmd/agent/cli/login.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing
- Produces:
  - `pub enum Provider { Anthropic, Chatgpt }` (derives `Debug, Clone, Copy, PartialEq, Eq`)
  - `pub fn Provider::parse(s: &str) -> Option<Provider>`
  - `pub fn Provider::label(self) -> &'static str`
  - `pub struct StoreStamp(String)` (derives `Debug, Clone, PartialEq, Eq`)
  - `pub fn store_stamp(p: Provider) -> Option<StoreStamp>`
  - `pub fn keychain_stamp_args() -> Vec<&'static str>` — exposed only so a test can assert `-w` is absent

- [ ] **Step 1: Write the failing tests**

Create `login.rs` with tests only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_aliases() {
        for s in ["anthropic", "Anthropic", "claude", "CLAUDE"] {
            assert_eq!(Provider::parse(s), Some(Provider::Anthropic), "{s}");
        }
        for s in ["chatgpt", "codex", "openai", "OpenAI"] {
            assert_eq!(Provider::parse(s), Some(Provider::Chatgpt), "{s}");
        }
        assert_eq!(Provider::parse("bogus"), None);
    }

    #[test]
    fn labels_are_user_facing_brand_spellings() {
        assert_eq!(Provider::Anthropic.label(), "Anthropic");
        assert_eq!(Provider::Chatgpt.label(), "ChatGPT");
    }

    #[test]
    fn keychain_read_never_asks_for_the_secret() {
        // `-w` is what makes `security` print the password itself. murmur reads
        // metadata only; this test is the guard on that promise.
        let args = keychain_stamp_args();
        assert!(!args.contains(&"-w"), "must not request the secret: {args:?}");
        assert!(args.contains(&"Claude Code-credentials"));
    }

    #[test]
    fn stamp_compares_by_equality_only() {
        let a = StoreStamp("20260823070925Z".into());
        let b = StoreStamp("20260823070925Z".into());
        let c = StoreStamp("20260823150000Z".into());
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn file_stamp_changes_when_the_file_is_touched() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("auth.json");
        std::fs::write(&f, "{}").unwrap();
        let first = file_stamp(&f).expect("stamp");
        // mtime resolution can be coarse; write different content after a
        // beat so the change is unambiguous.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&f, "{\"a\":1}").unwrap();
        assert_ne!(first, file_stamp(&f).expect("stamp"));
    }

    #[test]
    fn missing_file_has_no_stamp() {
        assert_eq!(file_stamp(std::path::Path::new("/nonexistent/auth.json")), None);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p mur-core cli::login`
Expected: FAIL — the module is not declared yet (`file not found for module 'login'` once `mod login;` is added, or no tests collected before that).

- [ ] **Step 3: Implement**

Prepend to `login.rs`:

```rust
//! `/login` — OAuth health and repair for the providers murmur reaches through
//! the model gateway.
//!
//! murmur never reads a token. Health comes from the owner CLI's own status
//! output; "did a refresh happen" comes from credential-store *metadata*. The
//! gateway is the only component that holds a credential, and it re-reads it
//! per request — so nothing here needs to restart an agent.

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Keychain service name Claude Code stores its credential under.
const CLAUDE_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    Chatgpt,
}

impl Provider {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "anthropic" | "claude" => Some(Self::Anthropic),
            "chatgpt" | "codex" | "openai" => Some(Self::Chatgpt),
            _ => None,
        }
    }

    /// User-facing label. Vendor spellings, not the wire-protocol names.
    pub fn label(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::Chatgpt => "ChatGPT",
        }
    }

    pub const ALL: [Provider; 2] = [Provider::Anthropic, Provider::Chatgpt];
}

/// An opaque marker for "the credential store as it stood at some moment".
/// Compared for equality only — never parsed, never displayed, never a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreStamp(String);

/// Arguments for the metadata read. Split out so a test can assert that `-w`
/// — the flag that would print the password itself — is never present.
pub fn keychain_stamp_args() -> Vec<&'static str> {
    vec!["find-generic-password", "-s", CLAUDE_KEYCHAIN_SERVICE]
}

// `dirs::home_dir()` — the crate mur-core already depends on, and the same
// resolution the runtime sandbox uses (see `cli/access.rs`). NOT
// `directories::BaseDirs`, which this crate does not depend on.
fn claude_credentials_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude/.credentials.json"))
}

fn codex_auth_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".codex/auth.json"))
}

/// mtime of a credential file, as an opaque stamp.
fn file_stamp(p: &Path) -> Option<StoreStamp> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    let d = m.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(StoreStamp(format!("{}.{}", d.as_secs(), d.subsec_nanos())))
}

/// The keychain item's `mdat` line, verbatim. `security` prints it without
/// `-w`, so no secret is read.
#[cfg(target_os = "macos")]
fn keychain_stamp() -> Option<StoreStamp> {
    let out = std::process::Command::new("security")
        .args(keychain_stamp_args())
        .output()
        .ok()?;
    // `security` writes the attribute dump to stderr.
    let text = String::from_utf8_lossy(&out.stderr).into_owned()
        + &String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find(|l| l.contains("\"mdat\""))
        .map(|l| StoreStamp(l.trim().to_string()))
}

#[cfg(not(target_os = "macos"))]
fn keychain_stamp() -> Option<StoreStamp> {
    None
}

/// Current stamp for a provider's credential store, or `None` when there is
/// no store to stamp (never logged in, or a platform without one).
pub fn store_stamp(p: Provider) -> Option<StoreStamp> {
    match p {
        // macOS keeps it in the keychain; Linux/Windows installs write a file.
        Provider::Anthropic => {
            keychain_stamp().or_else(|| claude_credentials_path().as_deref().and_then(file_stamp))
        }
        Provider::Chatgpt => codex_auth_path().as_deref().and_then(file_stamp),
    }
}
```

In `mod.rs`, add `mod login;` in the alphabetical run of module declarations (between `mod follow;` and `mod footer;`… place it after `mod footer;` to keep alphabetical order: `follow`, `footer`, `login`, `manage`).

If `tempfile` is not already a dev-dependency of `mur-core`, add it: `cargo add --dev tempfile -p mur-core`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p mur-core cli::login`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
cargo clippy -p mur-core --all-targets -- -D warnings
git add mur-core/src/cmd/agent/cli/login.rs mur-core/src/cmd/agent/cli/mod.rs mur-core/Cargo.toml
git commit -m "feat(murmur): credential-store metadata without reading the credential

'Did a refresh happen' is answerable from the store's modification time,
so murmur never has to hold a token. keychain_stamp_args is public purely
so a test can assert that -w — the flag that prints the password — never
appears in the argv."
```

---

### Task 3: Ask the owner CLI whether it is logged in

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/login.rs`
- Test: same file

**Interfaces:**
- Consumes: `Provider` (Task 2)
- Produces:
  - `pub struct OwnerStatus { pub logged_in: bool, pub identity: Option<String>, pub cli_present: bool }` (derives `Debug, Clone, PartialEq, Eq`)
  - `pub fn parse_claude_status(json: &str) -> OwnerStatus`
  - `pub fn parse_codex_status(text: &str) -> OwnerStatus`
  - `pub fn owner_status(p: Provider) -> OwnerStatus`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn claude_status_logged_in() {
        let json = r#"{"loggedIn":true,"authMethod":"claude.ai","email":"a@b.c","subscriptionType":"max"}"#;
        let s = parse_claude_status(json);
        assert!(s.logged_in);
        assert_eq!(s.identity.as_deref(), Some("a@b.c (max)"));
        assert!(s.cli_present);
    }

    #[test]
    fn claude_status_logged_out() {
        let s = parse_claude_status(r#"{"loggedIn":false}"#);
        assert!(!s.logged_in);
        assert_eq!(s.identity, None);
    }

    #[test]
    fn claude_status_without_subscription_still_shows_the_email() {
        let s = parse_claude_status(r#"{"loggedIn":true,"email":"a@b.c"}"#);
        assert_eq!(s.identity.as_deref(), Some("a@b.c"));
    }

    #[test]
    fn malformed_claude_status_is_not_a_panic() {
        // A CLI upgrade could change the shape. Degrade to "unknown", never crash.
        let s = parse_claude_status("not json at all");
        assert!(!s.logged_in);
        assert_eq!(s.identity, None);
    }

    #[test]
    fn codex_status_variants() {
        assert!(parse_codex_status("Logged in using ChatGPT").logged_in);
        assert!(!parse_codex_status("Not logged in").logged_in);
        assert!(!parse_codex_status("").logged_in);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p mur-core cli::login`
Expected: FAIL — `cannot find function 'parse_claude_status'`.

- [ ] **Step 3: Implement**

```rust
/// Timeout for a status call. These are local and answer immediately when
/// warm; the bound exists so a wedged CLI cannot freeze the TUI.
const STATUS_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerStatus {
    pub logged_in: bool,
    /// Display-only identity line, e.g. `"a@b.c (max)"`. Never a token.
    pub identity: Option<String>,
    /// False when the owner CLI is not installed — a supported state: a
    /// transplanted credential works without it, but cannot be repaired here.
    pub cli_present: bool,
}

impl OwnerStatus {
    fn unknown() -> Self {
        Self { logged_in: false, identity: None, cli_present: true }
    }
    fn absent() -> Self {
        Self { logged_in: false, identity: None, cli_present: false }
    }
}

/// Parse `claude auth status --json`. Any shape surprise degrades to
/// "not logged in", never to a panic — the CLI can change under us.
pub fn parse_claude_status(json: &str) -> OwnerStatus {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json.trim()) else {
        return OwnerStatus::unknown();
    };
    let logged_in = v.get("loggedIn").and_then(serde_json::Value::as_bool).unwrap_or(false);
    if !logged_in {
        return OwnerStatus::unknown();
    }
    let email = v.get("email").and_then(serde_json::Value::as_str);
    let sub = v.get("subscriptionType").and_then(serde_json::Value::as_str);
    let identity = match (email, sub) {
        (Some(e), Some(s)) => Some(format!("{e} ({s})")),
        (Some(e), None) => Some(e.to_string()),
        _ => None,
    };
    OwnerStatus { logged_in: true, identity, cli_present: true }
}

/// Parse `codex login status`, which prints prose rather than JSON.
pub fn parse_codex_status(text: &str) -> OwnerStatus {
    let t = text.trim();
    let logged_in = t.to_ascii_lowercase().starts_with("logged in");
    OwnerStatus {
        logged_in,
        identity: logged_in.then(|| t.to_string()),
        cli_present: true,
    }
}

fn run_capture(bin: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Ask the owner CLI. Blocking — callers dispatch it off the UI task.
pub fn owner_status(p: Provider) -> OwnerStatus {
    match p {
        Provider::Anthropic => run_capture("claude", &["auth", "status", "--json"])
            .map_or_else(OwnerStatus::absent, |s| parse_claude_status(&s)),
        Provider::Chatgpt => run_capture("codex", &["login", "status"])
            .map_or_else(OwnerStatus::absent, |s| parse_codex_status(&s)),
    }
}
```

`STATUS_TIMEOUT_SECS` is declared here and consumed in Task 5, where the call is wrapped; leaving it unused now would fail the clippy gate, so add `#[allow(dead_code)]` on it and remove that attribute in Task 5.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p mur-core cli::login`
Expected: PASS (11 tests).

- [ ] **Step 5: Commit**

```bash
cargo clippy -p mur-core --all-targets -- -D warnings
git add mur-core/src/cmd/agent/cli/login.rs
git commit -m "feat(murmur): read OAuth health from the owner CLIs

Parsing is total: a shape surprise from a CLI upgrade degrades to 'not
logged in' rather than panicking inside the TUI. cli_present distinguishes
'logged out' from 'no owner CLI installed', which is a supported state for
a transplanted credential."
```

---

### Task 4: Bare `/login` renders a status table

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/login.rs`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` — replace the placeholder dispatch arm
- Test: `mur-core/src/cmd/agent/cli/login.rs`

**Interfaces:**
- Consumes: `Provider`, `OwnerStatus`, `store_stamp`
- Produces: `pub fn render_status_line(p: Provider, s: &OwnerStatus, stamped: bool) -> String`, `pub fn render_status_all() -> String`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn status_line_shows_identity_when_logged_in() {
        let s = OwnerStatus { logged_in: true, identity: Some("a@b.c (max)".into()), cli_present: true };
        let line = render_status_line(Provider::Anthropic, &s, true);
        assert!(line.contains("Anthropic"), "{line}");
        assert!(line.contains("a@b.c (max)"), "{line}");
    }

    #[test]
    fn status_line_names_the_repair_when_logged_out() {
        let s = OwnerStatus { logged_in: false, identity: None, cli_present: true };
        let line = render_status_line(Provider::Chatgpt, &s, false);
        assert!(line.contains("/login chatgpt"), "must name the fix: {line}");
    }

    #[test]
    fn status_line_flags_a_missing_owner_cli() {
        // A transplanted credential can work while being unrepairable here.
        let s = OwnerStatus { logged_in: false, identity: None, cli_present: false };
        let line = render_status_line(Provider::Anthropic, &s, true);
        assert!(line.contains("not installed"), "{line}");
        assert!(!line.contains("/login anthropic"), "cannot repair without the CLI: {line}");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p mur-core cli::login::tests::status_line`
Expected: FAIL — `cannot find function 'render_status_line'`.

- [ ] **Step 3: Implement**

```rust
/// One provider's line in the `/login` table.
pub fn render_status_line(p: Provider, s: &OwnerStatus, stamped: bool) -> String {
    let label = p.label();
    if !s.cli_present {
        return format!(
            "  {label:<10} owner CLI not installed — credential cannot be repaired here"
        );
    }
    if s.logged_in {
        let who = s.identity.as_deref().unwrap_or("logged in");
        let store = if stamped { "" } else { "  (no credential store found)" };
        return format!("  {label:<10} ✓ {who}{store}");
    }
    let arg = match p {
        Provider::Anthropic => "anthropic",
        Provider::Chatgpt => "chatgpt",
    };
    format!("  {label:<10} not logged in — /login {arg}")
}

/// The whole table. Blocking; dispatch off the UI task.
pub fn render_status_all() -> String {
    let mut out = String::from("OAuth providers:\n");
    for p in Provider::ALL {
        let s = owner_status(p);
        out.push_str(&render_status_line(p, &s, store_stamp(p).is_some()));
        out.push('\n');
    }
    out.push_str(
        "\n(unrelated to `mur auth login`, which signs in to mur.run for the official catalog)",
    );
    out
}
```

In `mod.rs`, replace the placeholder arm. `render_status_all` shells out, so it goes through `spawn_blocking` — `run_manage` already does that, and its closure receives the agent name (unused here):

```rust
        SlashCmd::Login(arg) => {
            match arg {
                None => run_manage(app, move |_agent| Ok(login::render_status_all())).await,
                Some(word) => match login::Provider::parse(&word) {
                    None => app.push_error(format!(
                        "unknown provider {word:?} — try anthropic or chatgpt"
                    )),
                    Some(p) => login::dispatch_repair(app, p).await,
                },
            }
        }
```

`dispatch_repair` does not exist yet. For this task, stub it in `login.rs` so the tree builds; Task 5 fills it in:

```rust
/// Repair one provider. Filled in by the escalating-repair task.
pub async fn dispatch_repair(app: &mut crate::cmd::agent::cli::app::App, p: Provider) {
    app.push_system(format!("{}: repair not implemented yet", p.label()));
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p mur-core cli::login && cargo build -p mur-core`
Expected: PASS, and the crate builds.

- [ ] **Step 5: Verify by hand, then commit**

```bash
./build.sh --install
murmur <some-agent>     # then type: /login
```
Expected: both providers listed, each with either an identity or a `/login <provider>` hint.

```bash
git add -A mur-core/src/cmd/agent/cli/
git commit -m "feat(murmur): /login renders OAuth health for every provider

Reports all providers rather than only the running agent's — an agent's
model can change mid-session. Says plainly that it is unrelated to
mur auth login, which signs in to mur.run."
```

---

### Task 5: Escalating repair — stop at the cheapest rung that works

The common failure is an access token that aged out while the refresh token is still good. That needs no browser and no terminal handover: someone just has to ask the owner CLI to refresh. Only when that fails is a real login required.

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/login.rs`
- Test: same file

**Interfaces:**
- Consumes: `Provider`, `OwnerStatus`, `StoreStamp`, `store_stamp`, `owner_status`
- Produces:
  - `pub enum Rung { AlreadyHealthy, RefreshedByProbe, NeedsLogin, NoOwnerCli }` (derives `Debug, Clone, Copy, PartialEq, Eq`)
  - `pub fn classify_repair(before: Option<StoreStamp>, after: Option<StoreStamp>, status_after: &OwnerStatus) -> Rung`
  - `pub fn cheap_repair(p: Provider) -> Rung`

- [ ] **Step 1: Write the failing tests**

```rust
    fn st(logged_in: bool) -> OwnerStatus {
        OwnerStatus { logged_in, identity: None, cli_present: true }
    }

    #[test]
    fn a_moved_stamp_means_the_probe_repaired_it() {
        let before = Some(StoreStamp("a".into()));
        let after = Some(StoreStamp("b".into()));
        assert_eq!(classify_repair(before, after, &st(true)), Rung::RefreshedByProbe);
    }

    #[test]
    fn an_unmoved_stamp_with_a_healthy_status_was_already_fine() {
        let s = Some(StoreStamp("a".into()));
        assert_eq!(classify_repair(s.clone(), s, &st(true)), Rung::AlreadyHealthy);
    }

    #[test]
    fn an_unmoved_stamp_with_an_unhealthy_status_needs_a_real_login() {
        let s = Some(StoreStamp("a".into()));
        assert_eq!(classify_repair(s.clone(), s, &st(false)), Rung::NeedsLogin);
    }

    #[test]
    fn no_store_at_all_needs_a_real_login() {
        assert_eq!(classify_repair(None, None, &st(false)), Rung::NeedsLogin);
    }

    #[test]
    fn a_missing_owner_cli_cannot_be_repaired() {
        let absent = OwnerStatus { logged_in: false, identity: None, cli_present: false };
        assert_eq!(classify_repair(None, None, &absent), Rung::NoOwnerCli);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p mur-core cli::login::tests`
Expected: FAIL — `cannot find function 'classify_repair'`.

- [ ] **Step 3: Implement**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// Nothing was wrong, or something else had already fixed it.
    AlreadyHealthy,
    /// The owner CLI refreshed the credential. No browser, no handover.
    RefreshedByProbe,
    /// Only a real login will do — case B in the spec.
    NeedsLogin,
    /// The owner CLI is not installed; murmur cannot repair this here.
    NoOwnerCli,
}

/// Decide which rung the repair stopped at.
///
/// The stamp moving is positive proof the owner rewrote the credential. An
/// unmoved stamp is ambiguous on its own, so the owner's own health report
/// breaks the tie.
pub fn classify_repair(
    before: Option<StoreStamp>,
    after: Option<StoreStamp>,
    status_after: &OwnerStatus,
) -> Rung {
    if !status_after.cli_present {
        return Rung::NoOwnerCli;
    }
    if before != after && after.is_some() {
        return Rung::RefreshedByProbe;
    }
    if status_after.logged_in {
        Rung::AlreadyHealthy
    } else {
        Rung::NeedsLogin
    }
}

/// Rungs 1 and 2: re-read, then ask the owner CLI to refresh. Blocking.
/// Never opens a browser and never touches the terminal.
pub fn cheap_repair(p: Provider) -> Rung {
    let before = store_stamp(p);
    let status = owner_status(p);
    let after = store_stamp(p);
    classify_repair(before, after, &status)
}
```

Then replace the `dispatch_repair` stub from Task 4. Rungs 1–2 run off the UI task; rung 3 sets the handover flag consumed in Task 7:

```rust
/// Repair one provider, escalating only as far as needed.
pub async fn dispatch_repair(app: &mut crate::cmd::agent::cli::app::App, p: Provider) {
    let rung = match tokio::task::spawn_blocking(move || cheap_repair(p)).await {
        Ok(r) => r,
        Err(e) => {
            app.push_error(format!("login check failed: {e}"));
            return;
        }
    };
    match rung {
        Rung::AlreadyHealthy => app.push_system(format!(
            "{}: already authenticated — nothing to do",
            p.label()
        )),
        Rung::RefreshedByProbe => app.push_system(format!(
            "{}: refreshed ✓ — no restart needed, the gateway re-reads per request",
            p.label()
        )),
        Rung::NoOwnerCli => app.push_error(format!(
            "{}: owner CLI not installed — cannot re-authenticate from here",
            p.label()
        )),
        Rung::NeedsLogin => request_login_handover(app, p),
    }
}
```

`request_login_handover` is defined in Task 6 (it needs the headless check first). Add a temporary body so the tree builds:

```rust
fn request_login_handover(app: &mut crate::cmd::agent::cli::app::App, p: Provider) {
    app.push_system(format!("{}: needs a full login (not wired yet)", p.label()));
}
```

Remove the `#[allow(dead_code)]` from `STATUS_TIMEOUT_SECS` and apply it by wrapping `run_capture`'s child in a wait-with-timeout, killing the child on expiry.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p mur-core cli::login`
Expected: PASS (19 tests).

- [ ] **Step 5: Commit**

```bash
cargo clippy -p mur-core --all-targets -- -D warnings
git add mur-core/src/cmd/agent/cli/login.rs
git commit -m "feat(murmur): escalating repair, cheapest rung first

The every-8-hours failure is an aged-out access token with a live refresh
token. That needs no browser and no terminal handover — only someone to
ask the owner CLI to refresh. Routing it through a full OAuth flow, as the
first draft did, was the wrong repair for the common case.

A moved store stamp is positive proof the owner rewrote the credential; an
unmoved stamp is ambiguous, so the owner's own health report breaks the tie."
```

---

### Task 6: Headless — print the path that can actually complete

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/login.rs`
- Test: same file

**Interfaces:**
- Consumes: `Provider`, `Rung`
- Produces: `pub fn has_browser(env: &BrowserEnv) -> bool`, `pub struct BrowserEnv { pub macos: bool, pub display: bool, pub ssh: bool }`, `pub fn print_only_instructions(p: Provider) -> String`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn browser_detection_matrix() {
        let m = |macos, display, ssh| has_browser(&BrowserEnv { macos, display, ssh });
        // Local macOS desktop: always has a browser.
        assert!(m(true, false, false));
        // macOS reached over SSH: no usable browser on this end.
        assert!(!m(true, false, true));
        // Linux desktop.
        assert!(m(false, true, false));
        // Linux over SSH with X forwarding still works.
        assert!(m(false, true, true));
        // Headless Linux box.
        assert!(!m(false, false, false));
    }

    #[test]
    fn headless_instructions_name_a_command_that_works_without_a_browser() {
        let a = print_only_instructions(Provider::Anthropic);
        assert!(a.contains("claude setup-token"), "{a}");
        let c = print_only_instructions(Provider::Chatgpt);
        assert!(c.contains("--with-access-token") || c.contains("--with-api-key"), "{c}");
    }

    #[test]
    fn headless_instructions_mention_transplanting_a_credential() {
        // The other supported path: log in where a browser exists, copy it over.
        let a = print_only_instructions(Provider::Anthropic);
        assert!(a.contains(".credentials.json"), "{a}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p mur-core cli::login::tests::browser`
Expected: FAIL — `cannot find function 'has_browser'`.

- [ ] **Step 3: Implement**

```rust
/// The environment signals that decide whether a browser can open here.
/// Taken as data so the matrix is testable without touching the real env.
#[derive(Debug, Clone, Copy)]
pub struct BrowserEnv {
    pub macos: bool,
    /// `DISPLAY` or `WAYLAND_DISPLAY` is set.
    pub display: bool,
    /// `SSH_CONNECTION` is set.
    pub ssh: bool,
}

impl BrowserEnv {
    pub fn detect() -> Self {
        Self {
            macos: cfg!(target_os = "macos"),
            display: std::env::var_os("DISPLAY").is_some()
                || std::env::var_os("WAYLAND_DISPLAY").is_some(),
            ssh: std::env::var_os("SSH_CONNECTION").is_some(),
        }
    }
}

/// Heuristic, not proof — `--print-only` and `--force-browser` exist because
/// this can be wrong in both directions.
pub fn has_browser(env: &BrowserEnv) -> bool {
    if env.display {
        // An X/Wayland display works even when forwarded over SSH.
        return true;
    }
    env.macos && !env.ssh
}

/// What to do when no browser can open here. These are the paths the owner
/// CLIs document for exactly this case; murmur only relays them.
pub fn print_only_instructions(p: Provider) -> String {
    match p {
        Provider::Anthropic => "\
No browser available here. Two ways in:

  1. Long-lived token (needs a Claude subscription):
       claude setup-token

  2. Log in where a browser exists, then copy the credential over:
       ~/.claude/.credentials.json
     The gateway reads that path directly on Linux and Windows installs."
            .to_string(),
        Provider::Chatgpt => "\
No browser available here. Two ways in:

  1. Inject a credential from stdin:
       printenv OPENAI_API_KEY | codex login --with-api-key
       printenv CODEX_ACCESS_TOKEN | codex login --with-access-token

  2. Log in where a browser exists, then copy the credential over:
       ~/.codex/auth.json"
            .to_string(),
    }
}
```

Replace the temporary `request_login_handover` with the real gate:

```rust
/// Rung 3. Only reached when nothing cheaper worked.
fn request_login_handover(app: &mut crate::cmd::agent::cli::app::App, p: Provider) {
    if !has_browser(&BrowserEnv::detect()) {
        app.push_system(format!("{}:\n{}", p.label(), print_only_instructions(p)));
        return;
    }
    let argv = match p {
        Provider::Anthropic => vec!["claude".into(), "auth".into(), "login".into()],
        Provider::Chatgpt => vec!["codex".into(), "login".into()],
    };
    app.pending_handover = Some(crate::cmd::agent::cli::app::HandoverRequest {
        argv,
        label: p.label().to_string(),
    });
}
```

`HandoverRequest` and `App::pending_handover` are added in Task 7. Implement Task 7 next; the tree will not build until then.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p mur-core cli::login::tests::browser cli::login::tests::headless`
Expected: PASS. The crate does not build yet — that is expected and resolved by Task 7.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/cli/login.rs
git commit -m "feat(murmur): headless path prints what can actually complete

Launching a browser flow on a headless box produces a URL nobody can open.
Detect it and relay the credential-injection commands the owner CLIs
already document, plus the transplant path the gateway supports."
```

---

### Task 7: Terminal handover

**Files:**
- Create: `mur-core/src/cmd/agent/cli/handover.rs`
- Modify: `mur-core/src/cmd/agent/cli/app.rs` — `HandoverRequest`, `App::pending_handover`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` — `mod handover;`, main-loop hook
- Test: `mur-core/src/cmd/agent/cli/handover.rs`

**Interfaces:**
- Consumes: `App` (for the flag)
- Produces:
  - `pub struct HandoverRequest { pub argv: Vec<String>, pub label: String }` (derives `Debug, Clone, PartialEq, Eq`) — in `app.rs`
  - `pub App::pending_handover: Option<HandoverRequest>` — defaults `None`
  - `pub fn handover::run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, viewport_h: u16, req: &HandoverRequest) -> Result<std::process::ExitStatus>`

**Why this shape:** `handle_slash(app, cmd, tx)` has no access to `terminal` or `events`; the main loop owns both. The flag mirrors `App::should_quit` and `App::render_mode`, which are how every other command asks the loop to do something.

- [ ] **Step 1: Write the failing test**

Create `handover.rs` with tests only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_argv_is_split_into_program_and_args() {
        let req = crate::cmd::agent::cli::app::HandoverRequest {
            argv: vec!["claude".into(), "auth".into(), "login".into()],
            label: "Anthropic".into(),
        };
        let (prog, args) = split_argv(&req).expect("non-empty argv");
        assert_eq!(prog, "claude");
        assert_eq!(args, ["auth", "login"]);
    }

    #[test]
    fn empty_argv_is_rejected_rather_than_spawning_a_shell() {
        let req = crate::cmd::agent::cli::app::HandoverRequest {
            argv: vec![],
            label: "x".into(),
        };
        assert!(split_argv(&req).is_none());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p mur-core cli::handover`
Expected: FAIL — module not declared / `split_argv` not found.

- [ ] **Step 3: Implement**

Add to `app.rs`, beside the other `App` fields:

```rust
/// An interactive child the main loop must run with the terminal handed over.
/// Set by a slash command; taken and cleared by the loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoverRequest {
    pub argv: Vec<String>,
    /// What to name in the before/after system messages.
    pub label: String,
}
```

Add the field to `App` (`pub pending_handover: Option<HandoverRequest>`) and initialise it to `None` in every constructor.

Create `handover.rs`:

```rust
//! Hand the terminal to an interactive child, then take it back.
//!
//! murmur runs on the MAIN screen with a bottom-anchored `Viewport::Inline`
//! (see `TerminalGuard::enter`), so the child's output lands naturally in
//! scrollback above the viewport — no alternate screen is involved.
//!
//! Three things here are load-bearing and easy to get wrong:
//!
//! * The caller MUST drop the `EventStream` first. It owns stdin, and a child
//!   inheriting stdin would race murmur for the user's keystrokes — the paste
//!   prompt in an OAuth flow would eat characters.
//! * Re-entry happens in `Drop`, so a child that panics, exits non-zero, or is
//!   killed still leaves a usable terminal.
//! * Re-anchoring must NOT purge. `purge_and_reanchor` clears scrollback,
//!   which would erase the login transcript — including the URL the user still
//!   needs and any failure message.

use anyhow::{Context, Result};
use crossterm::cursor;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange,
};
use crossterm::terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode};
use crossterm::execute;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{self, Stdout, Write};
use std::process::ExitStatus;

use crate::cmd::agent::cli::app::HandoverRequest;

/// How many times to retry building the replacement terminal. The
/// cursor-position query inside `Terminal::with_options` needs crossterm's
/// internal event reader, and the just-dropped `EventStream`'s background
/// thread can hold that lock a beat longer — the same race
/// `purge_and_reanchor` already guards against.
const REANCHOR_RETRIES: usize = 20;

pub fn split_argv(req: &HandoverRequest) -> Option<(&str, &[String])> {
    let (first, rest) = req.argv.split_first()?;
    Some((first.as_str(), rest))
}

/// Restores raw mode and the terminal's mode set on drop, so an early return
/// or a panic inside the handover cannot leave the user in a broken shell.
struct Suspended;

impl Suspended {
    /// Give the terminal back to the shell. Mirrors `TerminalGuard::drop`.
    fn begin(viewport_h: u16) -> Result<Self> {
        // Move below the inline viewport so the child does not paint over it,
        // then clear only the visible rows from there down. FromCursorDown
        // does not touch scrollback.
        let rows = crossterm::terminal::size()?.1;
        execute!(
            io::stdout(),
            cursor::MoveTo(0, rows.saturating_sub(viewport_h)),
            Clear(ClearType::FromCursorDown),
            DisableBracketedPaste,
            DisableFocusChange,
            cursor::Show,
        )
        .context("release terminal modes")?;
        disable_raw_mode().context("disable raw mode")?;
        Ok(Self)
    }
}

impl Drop for Suspended {
    fn drop(&mut self) {
        let _ = enable_raw_mode();
        let _ = execute!(io::stdout(), EnableBracketedPaste, EnableFocusChange);
    }
}

/// Run `req` with the terminal handed over. The caller must have dropped the
/// `EventStream` and must recreate it afterwards.
pub fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    viewport_h: u16,
    req: &HandoverRequest,
) -> Result<ExitStatus> {
    let (prog, args) = split_argv(req).context("handover requested with empty argv")?;

    let status = {
        let _suspended = Suspended::begin(viewport_h)?;
        // Inherit all three handles: this is an interactive flow.
        std::process::Command::new(prog)
            .args(args)
            .status()
            .with_context(|| format!("run {prog}"))?
        // `_suspended` drops here — raw mode is back on before we redraw,
        // and stays restored even if `status()` returned Err above.
    };

    // Make room below whatever the child printed, then anchor a fresh inline
    // viewport there. No Clear(All), no Clear(Purge): the login transcript
    // stays in scrollback where the user can still read it.
    let mut out = io::stdout();
    for _ in 0..viewport_h {
        writeln!(out)?;
    }
    out.flush()?;

    let rows = crossterm::terminal::size()?.1;
    execute!(out, cursor::MoveTo(0, rows.saturating_sub(viewport_h)))?;

    let mut last_err = None;
    for _ in 0..REANCHOR_RETRIES {
        match Terminal::with_options(
            CrosstermBackend::new(io::stdout()),
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Inline(viewport_h),
            },
        ) {
            Ok(t) => {
                *terminal = t;
                return Ok(status);
            }
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    Err(anyhow::anyhow!(
        "could not re-anchor the viewport after handover: {}",
        last_err.map_or_else(|| "unknown".to_string(), |e| e.to_string())
    ))
}
```

Declare it in `mod.rs`: `mod handover;` (alphabetically after `mod footer;`, before `mod login;`).

Add the loop hook in `run_tui`, immediately after the slash command is dispatched and before the next draw:

```rust
        if let Some(req) = app.pending_handover.take() {
            app.push_system(format!("{}: handing over the terminal…", req.label));
            // The EventStream owns stdin; the child must have it to itself.
            drop(events);
            let outcome = handover::run(terminal, viewport_h, &req);
            events = EventStream::new();
            match outcome {
                Ok(s) if s.success() => {
                    app.push_system(format!(
                        "{}: logged in ✓ — no restart needed, the gateway re-reads per request",
                        req.label
                    ));
                }
                Ok(s) => app.push_error(format!("{}: login exited with {s}", req.label)),
                Err(e) => app.push_error(format!("{}: handover failed: {e:#}", req.label)),
            }
            app.needs_full_redraw = true;
        }
```

Place it in the same part of the loop body as the resize rebuild, which already establishes `drop(events)` → rebuild → `events = EventStream::new()`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p mur-core cli::handover && cargo build -p mur-core`
Expected: PASS, and the crate now builds (Task 6's forward reference is resolved).

- [ ] **Step 5: Manual verification — the part tests cannot reach**

```bash
./build.sh --install
murmur <some-agent>
```

Then, in the TUI:

1. `/login anthropic` while already logged in → expect `already authenticated`, **no** handover.
2. Force rung 3 by making `cheap_repair` return `Rung::NeedsLogin` temporarily, then `/login anthropic`:
   - the child takes the screen, murmur's viewport is gone
   - typing goes to the child, not to murmur's composer
   - on exit, the viewport comes back at the bottom
   - **scroll up: the login transcript is still there** (this is the regression `purge_and_reanchor` would have caused)
3. Repeat and press Ctrl-C during the child → murmur survives with a usable terminal.
4. Repeat and `kill -9` the child from another shell → same.
5. Resize the terminal after a handover → the viewport re-anchors without artifacts.

Revert the temporary `NeedsLogin` forcing.

- [ ] **Step 6: Commit**

```bash
cargo clippy -p mur-core --all-targets -- -D warnings
cargo fmt
git add -A mur-core/src/cmd/agent/cli/
git commit -m "feat(murmur): hand the terminal to an interactive login child

murmur is on the main screen with an inline viewport, so the child's output
lands in scrollback naturally — no alternate screen involved.

Three things are load-bearing: the EventStream is dropped first because it
owns stdin and would otherwise eat the user's pasted code; re-entry happens
in Drop so a panicking or killed child still leaves a usable terminal; and
re-anchoring deliberately does NOT reuse purge_and_reanchor, whose
Clear(Purge) would erase the login transcript the user may still need."
```

---

### Task 8: One login flow at a time

`murmur a1 a2 a3` runs one process per pane. Two concurrent `/login` calls would launch two OAuth flows against the same credential store.

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/login.rs`
- Test: same file

**Interfaces:**
- Consumes: `Provider`
- Produces: `pub struct LoginLock(std::fs::File)`, `pub fn acquire_login_lock(home: &Path) -> Option<LoginLock>`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn a_second_login_lock_is_refused_while_the_first_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let first = acquire_login_lock(dir.path()).expect("first lock");
        assert!(
            acquire_login_lock(dir.path()).is_none(),
            "a second flow must be refused while the first holds the lock"
        );
        drop(first);
        assert!(
            acquire_login_lock(dir.path()).is_some(),
            "the lock must be released on drop"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p mur-core login_lock`
Expected: FAIL — `cannot find function 'acquire_login_lock'`.

- [ ] **Step 3: Implement**

```rust
/// Held for the duration of an interactive login. Released on drop, including
/// on panic, so a crashed pane cannot wedge the others out.
pub struct LoginLock(std::fs::File);

/// Take the cross-pane login lock, or `None` if another pane holds it.
/// Advisory `flock`: the OS releases it if the process dies, which a
/// pid-file scheme would not.
pub fn acquire_login_lock(home: &Path) -> Option<LoginLock> {
    use std::os::unix::io::AsRawFd;
    let path = home.join("login.lock");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()?;
    // SAFETY: flock on a live fd this function owns.
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    (rc == 0).then_some(LoginLock(f))
}
```

Confirm `libc` is a dependency of `mur-core`; add it if not (`cargo add libc -p mur-core`).

Take the lock in `request_login_handover`, before setting the flag, and thread it into `HandoverRequest` so it is held for the child's lifetime — add a field:

```rust
pub struct HandoverRequest {
    pub argv: Vec<String>,
    pub label: String,
    /// Held for the child's lifetime; dropped with the request.
    pub _lock: Option<crate::cmd::agent::cli::login::LoginLock>,
}
```

Adding a field breaks every existing `HandoverRequest { .. }` literal — the two in Task 7's tests and the one in `request_login_handover`. Update all three in this task (add `_lock: None` to the test literals). `LoginLock` has no `PartialEq`, so also drop `PartialEq, Eq` from `HandoverRequest`'s derives; Task 7's tests compare `argv`/`label` directly and do not need them.

In `request_login_handover`:

```rust
    let Some(lock) = acquire_login_lock(&app.home) else {
        app.push_error(
            "another murmur pane is already running a login — finish that one first".into(),
        );
        return;
    };
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p mur-core cli::login cli::handover`
Expected: PASS.

- [ ] **Step 5: Manual verification and commit**

Open two panes on the same machine (`murmur a1` and `murmur a2`), force rung 3 in both, and confirm the second reports the lock rather than launching a second flow.

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo nextest run -p mur-core
git add -A
git commit -m "feat(murmur): serialise interactive logins across panes

murmur a1 a2 a3 is one process per pane, so two /login calls could launch
two OAuth flows against the same credential store. Advisory flock rather
than a pid file: the OS releases it if a pane dies."
```

---

## Documentation

After Task 8, per `CLAUDE.md`'s documentation checklist, update all three surfaces for the new command: `README.md`, the docs site, and the product page. Use the **`update-docs`** skill for the exact paths and publish steps — do not reconstruct them.

The CLI surface section of `CLAUDE.md` also lists murmur's in-session slash commands; add `/login` to that list.

## Self-review notes

Spec sections and the task that implements each:

| Spec section | Task |
|---|---|
| `/login`, `/login <provider>`, aliases | 1, 2 |
| Bare `/login` reports all providers | 4 |
| Distinguish from `mur auth login` | 1 (help), 4 (footer) |
| murmur never reads a secret | 2 (guard test), 3 |
| Rung 1–2 escalating repair | 5 |
| Rung 3 full login | 6, 7 |
| Terminal handover constraints | 7 |
| Headless / print-only | 6 |
| Single-flight | 8 |
| "No restart needed" messaging | 5, 7 |

Deliberately **not** implemented here: `--print-only` and `--force-browser` flags. `print_only_instructions` and `BrowserEnv` are already public and testable, so the flags are a small parse addition once someone needs them. Adding them now would be arguing with a heuristic no one has yet seen misfire.
