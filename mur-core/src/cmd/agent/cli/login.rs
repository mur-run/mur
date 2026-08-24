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
///
/// The **order** is the whole point: stamp, *then* probe, *then* stamp again.
/// Taking both stamps before the probe would compare a store against itself
/// and report `AlreadyHealthy`/`NeedsLogin` for a credential the probe just
/// repaired. `classify_repair`'s tests cannot see that — they are handed the
/// two stamps already taken — so the sequence gets its own seam and its own
/// test.
pub fn cheap_repair(p: Provider) -> Rung {
    cheap_repair_in(p, store_stamp, owner_status)
}

/// `cheap_repair`'s body with its two effects injected, so a test can control
/// what the store looks like before and after the probe. Production passes the
/// real `store_stamp`/`owner_status`; nothing else should call this.
pub(crate) fn cheap_repair_in(
    p: Provider,
    stamp: impl Fn(Provider) -> Option<StoreStamp>,
    status: impl Fn(Provider) -> OwnerStatus,
) -> Rung {
    let before = stamp(p);
    let status = status(p);
    let after = stamp(p);
    classify_repair(before, after, &status)
}

/// Repair one provider, escalating only as far as needed. Rungs 1-2
/// (`cheap_repair`) shell out, so they run off the UI task via
/// `spawn_blocking` — the same pattern `run_manage` uses for the rest of
/// `/login`'s profile-management calls.
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

/// Stub: rung 3 needs a real login. A later task adds the headless check and
/// the handover flow itself (`HandoverRequest` / `App::pending_handover`);
/// this task only routes here and says so.
fn request_login_handover(app: &mut crate::cmd::agent::cli::app::App, p: Provider) {
    app.push_system(format!("{}: needs a full login (not wired yet)", p.label()));
}

#[cfg(test)]
#[path = "login/tests.rs"]
mod tests;
