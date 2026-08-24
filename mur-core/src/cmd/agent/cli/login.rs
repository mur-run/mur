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

// `dirs::home_dir()` resolution happens once, in `store_stamp` below — the
// crate mur-core already depends on `dirs`, and this is the same resolution
// the runtime sandbox uses (see `cli/access.rs`). NOT `directories::BaseDirs`,
// which this crate does not depend on. These two helpers just join the
// per-provider suffix onto an already-resolved home, so `store_stamp_in`
// (below) can be driven with a temp-dir home in tests.
fn claude_credentials_path(home: &Path) -> PathBuf {
    home.join(".claude/.credentials.json")
}

fn codex_auth_path(home: &Path) -> PathBuf {
    home.join(".codex/auth.json")
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
    let text =
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find(|l| l.contains("\"mdat\""))
        .map(|l| StoreStamp(l.trim().to_string()))
}

#[cfg(not(target_os = "macos"))]
fn keychain_stamp() -> Option<StoreStamp> {
    None
}

/// [`store_stamp`]'s routing, with its two real-world inputs — the home
/// directory and the (macOS-only, machine-global) keychain probe — injected.
/// `store_stamp` is the thin production wrapper that resolves both for real
/// and delegates here.
///
/// This is the seam that closes `store_stamp`'s own test gap: before this,
/// nothing drove the function with a controllable home directory, so a test
/// could not tell "Anthropic reads the claude store" apart from "Anthropic
/// reads whatever store the Chatgpt arm happens to read" — swapping the two
/// match arms below passed every test in the file. `keychain` is forced
/// rather than shelled out to the real `security` binary: the real keychain
/// is machine-global state a test cannot control (and, on a box that has
/// ever logged into Claude Code, would make the Anthropic arm return a real
/// stamp regardless of `home`, masking exactly the bug this seam exists to
/// catch).
fn store_stamp_in(p: Provider, home: &Path, keychain: Option<StoreStamp>) -> Option<StoreStamp> {
    match p {
        // macOS keeps it in the keychain; Linux/Windows installs write a file.
        Provider::Anthropic => keychain.or_else(|| file_stamp(&claude_credentials_path(home))),
        Provider::Chatgpt => file_stamp(&codex_auth_path(home)),
    }
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
pub fn store_stamp(p: Provider) -> Option<StoreStamp> {
    // Keychain first, and lazily: `dirs::home_dir()` is only resolved if
    // the keychain doesn't already answer, matching the laziness the
    // pre-split code had inside its Anthropic match arm. Resolving
    // `home_dir()` unconditionally ahead of this check — an earlier
    // version of this function did exactly that — is a behavioral change,
    // not a refactor: it would return `None` whenever `home_dir()` fails
    // even though the keychain could still have answered. That never
    // surfaces as a false "not logged in" (the UI already hedges that
    // case — see `render_status_line`), only as a spurious "(no
    // credential store found)" next to an otherwise-correct "✓ ...".
    let keychain = match p {
        Provider::Anthropic => keychain_stamp(),
        Provider::Chatgpt => None,
    };
    keychain.or_else(|| store_stamp_in(p, &dirs::home_dir()?, None))
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
    /// The CLI ran and answered, but said (or implied) "not logged in" — OR
    /// the status check never got an answer at all because [`run_capture`]
    /// hit [`STATUS_TIMEOUT_SECS`] and killed a wedged process. Both
    /// collapse to this exact same value; nothing downstream of
    /// `run_capture` can tell them apart. `render_status_line` words its
    /// logged-out row to not overclaim, given that.
    fn unknown() -> Self {
        Self {
            logged_in: false,
            identity: None,
            cli_present: true,
        }
    }

    /// The CLI could not be run at all (not installed, or not on `PATH`).
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
/// human-readable text — matched case-insensitively (a capitalisation change
/// upstream must not silently start reporting "not logged in") and per line
/// (so a preamble line before the status line can't hide it either).
/// `identity` is set to the matching line only, trimmed — never the whole
/// blob — so it stays a single "identity line" as the field above documents.
///
/// `starts_with`, not `contains`: "Not logged in" contains the substring
/// "logged in" and must not match.
pub fn parse_codex_status(text: &str) -> OwnerStatus {
    let line = text
        .lines()
        .find(|l| l.trim_start().to_ascii_lowercase().starts_with("logged in"));
    match line {
        Some(l) => OwnerStatus {
            logged_in: true,
            identity: Some(l.trim().to_string()),
            cli_present: true,
        },
        None => OwnerStatus::unknown(),
    }
}

/// Timeout for a single owner-CLI status call. These calls are local and
/// answer immediately when the CLI is healthy; the bound exists so a wedged
/// CLI cannot freeze the TUI that calls [`owner_status`].
// Enforced in `run_capture` below, not deferred to Task 5: Task 4 wires
// `owner_status` into the live `/login` command via `spawn_blocking` with no
// timeout of its own, so deferring this would open a real hang window
// between Task 4 landing and Task 5 landing. It stays a separate constant
// because it is also the documented bound callers can rely on.
const STATUS_TIMEOUT_SECS: u64 = 15;

/// Runs `bin args...` and returns its stdout.
///
/// `None` only if the process could not be started at all (most commonly:
/// `bin` is not installed) — the one case `owner_status` maps to
/// [`OwnerStatus::absent`]. If the process does not exit within `timeout`, it
/// is killed and this still returns `Some` (whatever partial output it wrote,
/// or an empty string) rather than `None`: the CLI *is* present, it just
/// would not answer, so it must degrade through the same path as an empty or
/// malformed response — see `parse_claude_status`/`parse_codex_status` —
/// never through "not installed".
///
/// A non-zero exit is deliberately NOT treated as failure either — some CLIs
/// use it for "not logged in" and still print a parseable status to stdout.
fn run_capture(bin: &str, args: &[&str], timeout: std::time::Duration) -> Option<String> {
    use std::io::Read;

    let mut child = std::process::Command::new(bin)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    // Drain stdout on its own thread: the wait loop below polls rather than
    // blocking on the child, and an unread pipe can fill and stall the child
    // before it ever gets a chance to exit.
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Ok(None) => {
                // Wedged: kill so the reader thread's pipe closes, then fall
                // through to the same `Some(..)` return as a normal exit —
                // degrade like an empty response, not like "not installed".
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            Err(_) => return None,
        }
    }

    // A 1s safety net, not a second real timeout: once the child has exited
    // (or been killed) its pipe closes, so the reader thread's
    // `read_to_string` returns almost immediately.
    Some(
        rx.recv_timeout(std::time::Duration::from_secs(1))
            .unwrap_or_default(),
    )
}

/// Ask the owner CLI whether its credential is healthy.
///
/// Bounded by [`STATUS_TIMEOUT_SECS`] so a wedged CLI cannot freeze the TUI
/// that calls this.
pub fn owner_status(p: Provider) -> OwnerStatus {
    let timeout = std::time::Duration::from_secs(STATUS_TIMEOUT_SECS);
    match p {
        Provider::Anthropic => run_capture("claude", &["auth", "status", "--json"], timeout)
            .map(|out| parse_claude_status(&out))
            .unwrap_or_else(OwnerStatus::absent),
        Provider::Chatgpt => run_capture("codex", &["login", "status"], timeout)
            .map(|out| parse_codex_status(&out))
            .unwrap_or_else(OwnerStatus::absent),
    }
}

/// One provider's line in the `/login` table.
///
/// The `!logged_in && cli_present` branch cannot distinguish "the CLI
/// answered and said not logged in" from "the status check ran out of time"
/// — see [`OwnerStatus::unknown`]. Rather than assert a confident "not
/// logged in" that might actually be a wedged CLI, the wording hedges; the
/// repair hint (`/login <provider>`) is the right next step either way.
pub fn render_status_line(p: Provider, s: &OwnerStatus, stamped: bool) -> String {
    let label = p.label();
    if !s.cli_present {
        return format!(
            "  {label:<10} owner CLI not installed — credential cannot be repaired here"
        );
    }
    if s.logged_in {
        let who = s.identity.as_deref().unwrap_or("logged in");
        let store = if stamped {
            ""
        } else {
            "  (no credential store found)"
        };
        return format!("  {label:<10} ✓ {who}{store}");
    }
    let arg = match p {
        Provider::Anthropic => "anthropic",
        Provider::Chatgpt => "chatgpt",
    };
    format!("  {label:<10} not logged in (or the check timed out) — /login {arg}")
}

/// The whole table. Blocking (shells out per provider via [`owner_status`]
/// and, on macOS, [`store_stamp`]) — dispatch off the UI task.
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

/// Repair one provider. Filled in by the escalating-repair task.
pub async fn dispatch_repair(app: &mut crate::cmd::agent::cli::app::App, p: Provider) {
    app.push_system(format!("{}: repair not implemented yet", p.label()));
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

    #[test]
    fn codex_status_is_case_insensitive() {
        // Pins the union design: a capitalisation change upstream (the
        // brief's own concern) must not silently start reporting "not
        // logged in". Fails under the old case-sensitive `starts_with`.
        let s = parse_codex_status("logged in using ChatGPT");
        assert!(s.logged_in);
        assert_eq!(s.identity.as_deref(), Some("logged in using ChatGPT"));
    }

    #[test]
    fn run_capture_kills_a_wedged_process_and_degrades_like_empty_output() {
        // A real subprocess, not a mock: proves the kill-on-timeout path
        // itself, not just that a `Duration` value got threaded through.
        let (bin, args): (&str, &[&str]) = if cfg!(windows) {
            (
                "powershell",
                &["-NoProfile", "-Command", "Start-Sleep -Seconds 5"],
            )
        } else {
            ("/bin/sleep", &["5"])
        };
        let start = std::time::Instant::now();
        let out = run_capture(bin, args, std::time::Duration::from_millis(200));
        // The value alone doesn't prove the timeout fired: a `sleep 5` that
        // ran to completion would *also* eventually return `Some("")`, since
        // sleep prints nothing. The elapsed bound is what actually
        // distinguishes "killed at ~200ms" from "waited out the full sleep".
        assert!(
            start.elapsed() < std::time::Duration::from_secs(3),
            "did not time out promptly: {:?}",
            start.elapsed()
        );
        assert_eq!(out, Some(String::new()));
    }

    #[test]
    fn store_stamp_reads_each_providers_own_file_not_the_others() {
        // Regression pin for the acceptance criterion on `store_stamp`: this
        // must fail if the function's two match arms are swapped (Anthropic
        // wired to the codex file, Chatgpt to the claude file) — that swap
        // previously passed every test in this file. `keychain: None` drives
        // the Anthropic arm through its file fallback only; see
        // `store_stamp_in`'s doc comment for why the real keychain can't be
        // used here.
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/.credentials.json"), "claude").unwrap();
        let expected_claude = file_stamp(&home.join(".claude/.credentials.json")).expect("stamp");

        // A real, measurable mtime gap — otherwise two writes issued back to
        // back could land on the same filesystem-reported instant and the
        // two "expected" stamps below could coincide, letting the test pass
        // even after a swap.
        std::thread::sleep(std::time::Duration::from_millis(20));

        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(home.join(".codex/auth.json"), "codex").unwrap();
        let expected_codex = file_stamp(&home.join(".codex/auth.json")).expect("stamp");

        assert_ne!(
            expected_claude, expected_codex,
            "test setup must produce distinguishable stamps"
        );

        assert_eq!(
            store_stamp_in(Provider::Anthropic, home, None),
            Some(expected_claude),
            "Anthropic must read the claude store, not the codex one"
        );
        assert_eq!(
            store_stamp_in(Provider::Chatgpt, home, None),
            Some(expected_codex),
            "Chatgpt must read the codex store, not the claude one"
        );
    }

    #[test]
    fn store_stamp_prefers_the_keychain_probe_over_the_file_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        // A claude file exists too, so a precedence *reversal* (file-then-
        // keychain instead of keychain-then-file) would surface as a
        // mismatch here, not just as a param-ignored bug.
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/.credentials.json"), "{}").unwrap();

        let from_keychain = StoreStamp("keychain-mdat-line".into());
        assert_eq!(
            store_stamp_in(Provider::Anthropic, home, Some(from_keychain.clone())),
            Some(from_keychain),
            "a keychain hit must win over the file fallback, even when a file exists too"
        );
    }

    #[test]
    fn status_line_shows_identity_when_logged_in() {
        let s = OwnerStatus {
            logged_in: true,
            identity: Some("a@b.c (max)".into()),
            cli_present: true,
        };
        let line = render_status_line(Provider::Anthropic, &s, true);
        assert!(line.contains("Anthropic"), "{line}");
        assert!(line.contains("a@b.c (max)"), "{line}");
    }

    #[test]
    fn status_line_names_the_repair_when_logged_out() {
        let s = OwnerStatus {
            logged_in: false,
            identity: None,
            cli_present: true,
        };
        let line = render_status_line(Provider::Chatgpt, &s, false);
        assert!(line.contains("/login chatgpt"), "must name the fix: {line}");
    }

    #[test]
    fn status_line_flags_a_missing_owner_cli() {
        // A transplanted credential can work while being unrepairable here.
        let s = OwnerStatus {
            logged_in: false,
            identity: None,
            cli_present: false,
        };
        let line = render_status_line(Provider::Anthropic, &s, true);
        assert!(line.contains("not installed"), "{line}");
        assert!(
            !line.contains("/login anthropic"),
            "cannot repair without the CLI: {line}"
        );
    }
}
