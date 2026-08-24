//! `/login` — OAuth health and repair for the providers murmur reaches through
//! the model gateway.
//!
//! murmur never reads a token. Health comes from the owner CLI's own status
//! output; "did a refresh happen" comes from credential-store *metadata*. The
//! gateway is the only component that holds a credential, and it re-reads it
//! per request — so nothing here needs to restart an agent.

use serde_json::Value;
use std::path::{Path, PathBuf};

/// Keychain service name Claude Code stores its credential under.
const CLAUDE_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    Chatgpt,
}

// Task 4 is the first caller of every item here (`Provider::parse` resolves
// the `/login <word>` argument, `.label()` renders it, `Provider::ALL` drives
// the no-argument status view) — until then the bin target (unlike the lib,
// which blankets `cmd` with `#[allow(dead_code)]`) sees this impl as unused.
#[expect(dead_code)]
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

    // Task 4 iterates this to render a status line per provider.
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

// Only reached through `store_stamp`, which nothing calls until Task 4.
#[expect(dead_code)]
fn codex_auth_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".codex/auth.json"))
}

/// mtime of a credential file, as an opaque stamp.
// Exercised directly by the tests below, but only reached from prod code
// through `store_stamp` — dead in the non-test bin build until Task 4 wires
// `store_stamp` in. The unit tests below call it directly, so `dead_code`
// never fires for it in a `#[cfg(test)]` build — only `cfg_attr(not(test),
// ...)` it, matching the existing idiom (`cmd/hook.rs::should_skip`,
// `server/mod.rs::build_router`), or a bare `#[expect]` goes unfulfilled and
// `-D warnings` hard-errors on `lib test` / `bin "mur" test`.
#[cfg_attr(not(test), expect(dead_code))]
fn file_stamp(p: &Path) -> Option<StoreStamp> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    let d = m.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(StoreStamp(format!("{}.{}", d.as_secs(), d.subsec_nanos())))
}

/// The keychain item's `mdat` line, verbatim. `security` prints it without
/// `-w`, so no secret is read.
// Only reached through `store_stamp`, which nothing calls until Task 4.
#[expect(dead_code)]
#[cfg(target_os = "macos")]
fn keychain_stamp() -> Option<StoreStamp> {
    let out = std::process::Command::new("security")
        .args(keychain_stamp_args())
        .output()
        .ok()?;
    // `security` writes the attribute dump to stderr.
    let text =
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find(|l| l.contains("\"mdat\""))
        .map(|l| StoreStamp(l.trim().to_string()))
}

#[expect(dead_code)]
#[cfg(not(target_os = "macos"))]
fn keychain_stamp() -> Option<StoreStamp> {
    None
}

/// Current stamp for a provider's credential store, or `None` when there is
/// no store to stamp.
///
/// **Known gap.** On Linux the credential can live in a keychain (the gateway
/// reads it through `keyring`'s linux-native backend), and murmur has no way
/// to stamp that without taking a dependency on the secret store itself. There
/// `store_stamp` returns `None`, so rung 2 can never report
/// `RefreshedByProbe`. That degrades correctly rather than lying —
/// `classify_repair` falls through to the owner CLI's own health report — but
/// it is a real limitation, not an oversight, and `no_stamp_degrades_to_the_cli_report`
/// pins the degradation.
// Task 4 is the first caller (`store_stamp(p).is_some()` per provider); Task
// 5's `classify_repair` is the second. Remove this attribute once Task 4
// lands.
#[expect(dead_code)]
pub fn store_stamp(p: Provider) -> Option<StoreStamp> {
    match p {
        // macOS keeps it in the keychain; Linux/Windows installs write a file.
        Provider::Anthropic => {
            keychain_stamp().or_else(|| claude_credentials_path().as_deref().and_then(file_stamp))
        }
        Provider::Chatgpt => codex_auth_path().as_deref().and_then(file_stamp),
    }
}

/// Health of a provider's credential, as reported by the CLI that owns it.
///
/// murmur never reads the token itself — this is the owner CLI's own
/// account-status output, reduced to what the TUI needs to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerStatus {
    pub logged_in: bool,
    /// Display-only identity line, e.g. `"a@b.c (max)"`. Never a token.
    pub identity: Option<String>,
    /// False when the owner CLI itself could not be run — a supported state:
    /// a transplanted credential still works through the gateway, but murmur
    /// cannot repair it here without the CLI that owns it.
    pub cli_present: bool,
}

impl OwnerStatus {
    /// The CLI ran and answered, but said (or implied) "not logged in".
    // Called from both parse fns below, so it shares their reachability: live
    // under `cfg(test)` (their tests reach it transitively), dead in the
    // non-test build until Task 4/5 call `owner_status`.
    #[cfg_attr(not(test), expect(dead_code))]
    fn unknown() -> Self {
        Self {
            logged_in: false,
            identity: None,
            cli_present: true,
        }
    }

    /// The CLI could not be run at all (not installed, or not on `PATH`).
    // Only reached from `owner_status`'s `run_capture` miss, and no test
    // exercises that subprocess path — dead in every build until Task 4.
    #[expect(dead_code)]
    fn absent() -> Self {
        Self {
            logged_in: false,
            identity: None,
            cli_present: false,
        }
    }
}

/// Parses `claude auth status --json`. Total: any shape surprise (a CLI
/// upgrade, a truncated pipe) degrades to "not logged in" rather than
/// panicking inside a running TUI.
// Exercised directly by the tests below; only reached from prod code through
// `owner_status`, which nothing calls until Task 4 — same `cfg_attr` idiom as
// `file_stamp` above, for the same reason.
#[cfg_attr(not(test), expect(dead_code))]
pub fn parse_claude_status(json: &str) -> OwnerStatus {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return OwnerStatus::unknown();
    };
    if !v.get("loggedIn").and_then(Value::as_bool).unwrap_or(false) {
        return OwnerStatus::unknown();
    }
    let email = v.get("email").and_then(Value::as_str);
    let sub = v.get("subscriptionType").and_then(Value::as_str);
    let identity = email.map(|e| match sub {
        Some(s) => format!("{e} ({s})"),
        None => e.to_string(),
    });
    OwnerStatus {
        logged_in: true,
        identity,
        cli_present: true,
    }
}

/// Parses `codex login status`. There is no `--json` form, so this reads the
/// human-readable text — anchored on line start and case-sensitive so "Not
/// logged in" can never match a `starts_with("Logged in")` check.
#[cfg_attr(not(test), expect(dead_code))]
pub fn parse_codex_status(text: &str) -> OwnerStatus {
    let logged_in = text
        .lines()
        .any(|l| l.trim_start().starts_with("Logged in"));
    OwnerStatus {
        logged_in,
        identity: logged_in.then(|| text.trim().to_string()),
        cli_present: true,
    }
}

/// Timeout for a single owner-CLI status call. These calls are local and
/// answer immediately when the CLI is healthy; the bound exists so a wedged
/// CLI cannot freeze the TUI.
// Not enforced here — Task 5 wraps the `owner_status` call with it. Nothing
// reads it until then.
#[expect(dead_code)]
const STATUS_TIMEOUT_SECS: u64 = 15;

/// Runs `bin args...` and returns its stdout, or `None` if the process could
/// not be started at all (most commonly: `bin` is not installed).
///
/// A non-zero exit is deliberately NOT treated as "absent" — some CLIs use it
/// for "not logged in" and still print a parseable status to stdout. The
/// parse functions above degrade unparseable or empty output on their own, so
/// there is no need to inspect the exit code here.
#[expect(dead_code)]
fn run_capture(bin: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(bin).args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Ask the owner CLI whether its credential is healthy.
///
/// Runs synchronously with no timeout of its own — a caller on a UI thread
/// should bound it with [`STATUS_TIMEOUT_SECS`].
#[expect(dead_code)]
pub fn owner_status(p: Provider) -> OwnerStatus {
    match p {
        Provider::Anthropic => run_capture("claude", &["auth", "status", "--json"])
            .map(|out| parse_claude_status(&out))
            .unwrap_or_else(OwnerStatus::absent),
        Provider::Chatgpt => run_capture("codex", &["login", "status"])
            .map(|out| parse_codex_status(&out))
            .unwrap_or_else(OwnerStatus::absent),
    }
}

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
        assert!(
            !args.contains(&"-w"),
            "must not request the secret: {args:?}"
        );
        assert!(args.contains(&"Claude Code-credentials"));
    }

    #[test]
    fn stamp_tracks_the_store_not_the_clock() {
        // Deliberately NOT `StoreStamp("a") == StoreStamp("a")` — that would
        // exercise `#[derive(PartialEq)]` and would still pass if `store_stamp`
        // returned a constant. Drive the real function instead: the same
        // untouched store must stamp identically, and a changed one must not.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("auth.json");
        std::fs::write(&f, "{}").unwrap();
        let first = file_stamp(&f).expect("stamp");
        assert_eq!(file_stamp(&f).expect("stamp"), first, "unchanged store");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&f, "{\"a\":1}").unwrap();
        assert_ne!(file_stamp(&f).expect("stamp"), first, "changed store");
    }

    #[test]
    fn missing_file_has_no_stamp() {
        assert_eq!(
            file_stamp(std::path::Path::new("/nonexistent/auth.json")),
            None
        );
    }

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
    fn claude_status_logged_out_ignores_identity_fields_when_present() {
        // `{"loggedIn":false}` alone can't tell whether the empty identity
        // came from the early return or from email/sub simply being absent.
        // Carry them anyway: a stale-but-present email must not leak into
        // the identity line once loggedIn says false.
        let s =
            parse_claude_status(r#"{"loggedIn":false,"email":"a@b.c","subscriptionType":"max"}"#);
        assert!(!s.logged_in);
        assert_eq!(s.identity, None);
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

    #[test]
    fn codex_identity_is_the_status_line_only_when_logged_in() {
        let in_ = parse_codex_status("Logged in using ChatGPT");
        assert_eq!(in_.identity.as_deref(), Some("Logged in using ChatGPT"));
        let out = parse_codex_status("Not logged in");
        assert_eq!(out.identity, None);
    }
}
