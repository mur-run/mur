//! `/login` — OAuth health and repair for the providers murmur reaches through
//! the model gateway.
//!
//! murmur never reads a token. Health comes from the owner CLI's own status
//! output; "did a refresh happen" comes from credential-store *metadata*. The
//! gateway is the only component that holds a credential, and it re-reads it
//! per request — so nothing here needs to restart an agent.

use serde_json::Value;
use std::path::{Path, PathBuf};

// The next two items are live ONLY on macOS and under `cfg(test)`: their sole
// non-test consumer is `keychain_stamp`, which is `#[cfg(target_os = "macos")]`.
// Off macOS the stub replaces it and nothing outside the test module reads
// either one, so both need `dead_code` suppression — but only exactly there,
// hence `cfg_attr` rather than a bare attribute.
//
// Three things about that gate, each learned the hard way:
//
// * `not(test)` is load-bearing. The `-w` guard test calls
//   `keychain_stamp_args`, so under `cfg(test)` the items ARE live and an
//   unconditional `expect` is *unfulfilled* — which `-D warnings` rejects via
//   `unfulfilled_lint_expectations`, failing `lib test` and `bin "mur" test`.
// * `expect`, not `allow`: if either ever gains a non-macOS caller, the
//   expectation fails the build and asks to be removed. An `allow` would sit
//   here forever.
// * It is the **bin** target that reports this, not the lib — inside a binary
//   nothing is externally reachable, so `pub` grants no reprieve. A
//   `cargo clippy --lib` on a non-macOS host is clean and proves nothing;
//   reproducing this needs `--all-targets`.

/// Keychain service name Claude Code stores its credential under.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(
        dead_code,
        reason = "read only by `keychain_stamp_args`, which is itself dead off macOS"
    )
)]
const CLAUDE_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// Where Claude Code writes its credential when it is not using a keychain
/// (Linux and Windows installs), relative to the home directory. Named once:
/// `claude_credentials_path` joins it and `print_only_instructions` quotes it
/// to the user, and those two drifting apart would send someone to a path the
/// stamp never reads.
const CLAUDE_CREDENTIALS_REL: &str = ".claude/.credentials.json";

/// Where Codex writes its credential, relative to the home directory. Same
/// two consumers as [`CLAUDE_CREDENTIALS_REL`].
const CODEX_AUTH_REL: &str = ".codex/auth.json";

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

    /// The CLI that owns this provider's credential: what murmur shells out
    /// to for health ([`owner_status`]), what it hands the terminal to at
    /// rung 3, and what [`render_status_line`] names when it is missing.
    pub fn owner_cli(self) -> &'static str {
        match self {
            Self::Anthropic => "claude",
            Self::Chatgpt => "codex",
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
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(
        dead_code,
        reason = "the only non-test caller is the macOS-only `keychain_stamp`, \
                  which is not compiled here"
    )
)]
pub fn keychain_stamp_args() -> Vec<&'static str> {
    vec!["find-generic-password", "-s", CLAUDE_KEYCHAIN_SERVICE]
}

// These two helpers just join the per-provider suffix onto an already-resolved
// home, so `store_stamp_in` (below) can be driven with a temp-dir home in
// tests. `dirs::home_dir` is the resolver production hands in — the crate
// mur-core already depends on `dirs`, and this is the same resolution the
// runtime sandbox uses (see `cli/access.rs`). NOT `directories::BaseDirs`,
// which this crate does not depend on.
fn claude_credentials_path(home: &Path) -> PathBuf {
    home.join(CLAUDE_CREDENTIALS_REL)
}

fn codex_auth_path(home: &Path) -> PathBuf {
    home.join(CODEX_AUTH_REL)
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

/// The whole of [`store_stamp`]'s behaviour, with its two real-world inputs
/// — the home directory and the (macOS-only, machine-global) keychain probe —
/// injected **as thunks**, not as values.
///
/// Thunks, because laziness is part of the routing and therefore has to live
/// on this side of the seam:
///
/// * the Chatgpt arm must never run the keychain probe (that shells out to
///   `security`), and
/// * a keychain hit must not need the home directory resolved at all, so a
///   box where `home_dir()` fails still gets a stamp.
///
/// An earlier version composed those two rules in the production wrapper
/// instead, which left the wrapper passing `keychain: None` unconditionally:
/// the `keychain` arm here became dead in production, the test that pinned it
/// pinned a path production never took, and `store_stamp` itself — the thing
/// actually called — had no test at all. Everything that decides *which store
/// a provider reads* now lives in this one `match`, under test
/// (`store_stamp_reads_each_providers_own_file_not_the_others` reddens if the
/// arms are swapped; `the_seam_resolves_only_what_it_needs` reddens if either
/// thunk is forced eagerly). What is left in `store_stamp` is a two-argument
/// delegation whose arguments cannot be transposed — they have different
/// return types.
///
/// The keychain is forced here rather than shelled out to for real in tests:
/// the real keychain is machine-global state a test cannot control (and, on a
/// box that has ever logged into Claude Code, would make the Anthropic arm
/// return a real stamp regardless of `home`, masking exactly the bug this seam
/// exists to catch).
fn store_stamp_in(
    p: Provider,
    home: impl FnOnce() -> Option<PathBuf>,
    keychain: impl FnOnce() -> Option<StoreStamp>,
) -> Option<StoreStamp> {
    match p {
        // macOS keeps it in the keychain; Linux/Windows installs write a file.
        Provider::Anthropic => {
            keychain().or_else(move || file_stamp(&claude_credentials_path(&home()?)))
        }
        Provider::Chatgpt => file_stamp(&codex_auth_path(&home()?)),
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
    store_stamp_in(p, dirs::home_dir, keychain_stamp)
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

/// Longest identity murmur will echo from an owner CLI into the transcript.
///
/// Wide enough for `email (subscription)` and for `codex login status`'s one
/// line. NOT a one-row bound: at 80 columns this is two wrapped rows, three
/// if the text is CJK. It is a damage bound, not a layout bound — the job is
/// to stop an upstream surprise from pasting a screenful into a
/// credential-adjacent UI, and the transcript wraps rather than truncating,
/// so a couple of rows is a cost worth paying to keep a long-but-legitimate
/// identity readable.
const MAX_IDENTITY_CHARS: usize = 120;

/// Bound an owner CLI's text before it becomes a transcript row.
///
/// The identity line is upstream output murmur does not control, and it ends
/// up in the transcript and then in the terminal's own scrollback. Two things
/// are therefore taken off it: unbounded length, and the characters
/// `char::is_control` matches — Unicode `Cc`, which is the class that carries
/// the real hazard (a stray `ESC` reaching a ratatui cell is terminal
/// injection, not a formatting quirk).
///
/// Deliberately narrower than "sanitised". Bidi and format characters
/// (`U+202E`, `U+200F`, the `Cf` class generally) are NOT stripped: they can
/// make a line display misleadingly, but they reach the terminal as ordinary
/// text and cannot drive it. Widening the filter is a behaviour change, not a
/// comment fix, so it is named here rather than quietly implied.
///
/// `codex login status` has no `--json`, so there is no field to select and
/// no shape to validate — bounding the damage is what is left.
fn sanitize_identity(line: &str) -> String {
    let clean: String = line.trim().chars().filter(|c| !c.is_control()).collect();
    match clean.char_indices().nth(MAX_IDENTITY_CHARS) {
        Some((i, _)) => format!("{}…", &clean[..i]),
        None => clean,
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
    // Selected fields, but still upstream strings — same bound as the codex
    // path, so there is one answer to "what can reach the transcript".
    let identity = email.map(|e| {
        sanitize_identity(&match sub {
            Some(s) => format!("{e} ({s})"),
            None => e.to_string(),
        })
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
/// `identity` is the matching line only, never the whole blob, and passed
/// through [`sanitize_identity`] so an arbitrary upstream line cannot reach
/// the transcript verbatim.
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
            identity: Some(sanitize_identity(l)),
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
/// `None` essentially always means the process could not be started at all
/// (most commonly: `bin` is not installed) — the one case `owner_status` maps
/// to [`OwnerStatus::absent`]. Once the spawn has succeeded, every *wait*
/// outcome returns `Some`: whatever partial output was written, or an empty
/// string. The CLI *is* present, so it must degrade through the same path as
/// an empty or malformed response — see
/// `parse_claude_status`/`parse_codex_status` — never through "not
/// installed", which becomes `Rung::NoOwnerCli` and tells the user to install
/// a CLI that is already there.
///
/// "essentially", not "only": `child.stdout.take()?` below is one more
/// `?`-to-`None` after a successful spawn, and it drops the child without
/// reaping it. It is unreachable as written — `Stdio::piped()` guarantees the
/// handle is there and nothing takes it first — so it is left alone rather
/// than given a fourth code path to keep in step with the other three. It is
/// named here so the sentence above is not read as a guarantee the code does
/// not make.
///
/// A non-zero exit is deliberately NOT treated as failure either — some CLIs
/// use it for "not logged in" and still print a parseable status to stdout.
fn run_capture(bin: &str, args: &[&str], timeout: std::time::Duration) -> Option<String> {
    use std::io::Read;

    let mut child = std::process::Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
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
            // Wedged past the deadline, or `try_wait` itself failed so we can
            // no longer tell whether it is alive. One arm, because the two
            // need the identical handling and a separate `Err` arm is exactly
            // where they drifted: returning `None` here both misreported a
            // running CLI as "not installed" and leaked the child and the
            // reader thread. Kill so the reader thread's pipe closes, reap,
            // then fall through to the same `Some(..)` return as a normal
            // exit.
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
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
    let bin = p.owner_cli();
    match p {
        Provider::Anthropic => run_capture(bin, &["auth", "status", "--json"], timeout)
            .map(|out| parse_claude_status(&out))
            .unwrap_or_else(OwnerStatus::absent),
        Provider::Chatgpt => run_capture(bin, &["login", "status"], timeout)
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
        // Deliberately not worded as a failure. Bare `/login` lists every
        // provider the gateway can reach, not the ones this agent uses (the
        // spec: an agent's model can change mid-session), so most users will
        // always see one row for a provider they have never touched — and
        // "credential cannot be repaired here" read like an error report for
        // a provider that is simply not in play. State the observation and
        // stop: no ✗, and no `/login <provider>` hint murmur could not honour
        // without the CLI anyway.
        return format!(
            "  {label:<10} — `{}` not installed, so nothing to check here",
            p.owner_cli()
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
pub fn render_status_all(agent: &str) -> String {
    let mut out = String::from("OAuth providers — the owner CLI's login:\n");
    for p in Provider::ALL {
        let s = owner_status(p);
        out.push_str(&render_status_line(p, &s, store_stamp(p).is_some()));
        out.push('\n');
    }
    out.push_str(&agent_endpoint_line(agent));
    out.push_str(
        "\n(unrelated to `mur auth login`, which signs in to mur.run for the official catalog)",
    );
    out
}

/// What this agent's next turn will actually dial.
///
/// The table above is about the owner CLI's login. An agent reaches its model
/// through the registry — provider, base URL, credential — and the two coincide
/// only when the agent happens to use OAuth through that CLI. On any other
/// setup a green tick above says nothing about whether the next turn works,
/// which is how a healthy-looking `/login` came to sit next to a 401 (#1100).
///
/// The secret is NAMED, never resolved: a status line must not pop a keychain
/// prompt. `mur agent doctor` resolves it deliberately, because it is
/// interactive and a human asked it to check.
fn agent_endpoint_line(agent: &str) -> String {
    // Through the canonicaliser, not straight into a path: agent lookup is
    // case-insensitive on the CLI (CLAUDE.md rule 8), and a typed name that
    // differs only in case would otherwise silently render nothing here. It
    // also removes the question of what a name containing `..` would do,
    // rather than leaving it to be reasoned about (#1106).
    let home = crate::paths::mur_root(None);
    let agent = crate::a2a_dial::canonicalize_agent_name(&home, agent);
    let path = home.join("agents").join(&agent).join("profile.yaml");
    let Ok(yaml) = std::fs::read_to_string(&path) else {
        return String::new();
    };
    let Ok(profile) = serde_yaml_ng::from_str::<mur_common::AgentProfile>(&yaml) else {
        return String::new();
    };
    // The runtime's own resolution, so this cannot describe a different model
    // than the one the turn will use.
    match mur_agent_runtime::supervisor::resolve_model_entry(&profile) {
        Ok(entry) => {
            let endpoint = entry
                .base_url
                .as_deref()
                .unwrap_or("the provider's default endpoint");
            // `label`, not `to_string`: a `cmd:` reference Displays its whole
            // command line, which is where an inline credential lives.
            let credential = match &entry.secret {
                Some(s) => s.label(),
                None => "the agent's own credentials".to_string(),
            };
            format!(
                "\n\nagent '{agent}' dials {endpoint} as {}/{}, carrying {credential}.\n\
                 A ✓ above covers that only if this agent goes through the owner CLI.",
                entry.provider, entry.model
            )
        }
        Err(e) => format!("\n\nagent '{agent}' has no resolvable model: {e}"),
    }
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
        Self::detect_with(cfg!(target_os = "macos"), |name| {
            std::env::var_os(name).is_some()
        })
    }

    /// `detect`'s body with the environment lookup injected, so a test can pin
    /// **which variable names are read** and how each maps onto a field.
    ///
    /// `has_browser` is pure and fully covered by its matrix, but that matrix
    /// is handed a `BrowserEnv` already built — it cannot see this function at
    /// all. Swap `DISPLAY` and `SSH_CONNECTION` here and every existing test
    /// still passes, while a headless SSH box is told it has a browser and a
    /// local desktop is told it does not.
    pub(crate) fn detect_with(macos: bool, has_var: impl Fn(&str) -> bool) -> Self {
        Self {
            macos,
            display: has_var("DISPLAY") || has_var("WAYLAND_DISPLAY"),
            ssh: has_var("SSH_CONNECTION"),
        }
    }
}

/// Heuristic, not proof — it can be wrong in both directions (a display that
/// won't actually open a usable browser, or a viable one this misses), so a
/// user would need a way to override it. No override exists yet.
pub fn has_browser(env: &BrowserEnv) -> bool {
    if env.display {
        // An X/Wayland display works even when forwarded over SSH.
        return true;
    }
    env.macos && !env.ssh
}

/// What to do when no browser can open here. These are the paths the owner
/// CLIs document for exactly this case; murmur only relays them.
///
/// The credential paths are interpolated from the same constants
/// [`claude_credentials_path`] and [`codex_auth_path`] join, so what the user
/// is told to copy a file to is by construction the file the stamp reads.
pub fn print_only_instructions(p: Provider) -> String {
    match p {
        Provider::Anthropic => format!(
            "\
No browser available here. Two ways in:

  1. Long-lived token (needs a Claude subscription):
       claude setup-token

  2. Log in where a browser exists, then copy the credential over:
       ~/{CLAUDE_CREDENTIALS_REL}
     The gateway reads that path directly on Linux and Windows installs."
        ),
        Provider::Chatgpt => format!(
            "\
No browser available here. Two ways in:

  1. Inject a credential from stdin:
       printenv OPENAI_API_KEY | codex login --with-api-key
       printenv CODEX_ACCESS_TOKEN | codex login --with-access-token

  2. Log in where a browser exists, then copy the credential over:
       ~/{CODEX_AUTH_REL}"
        ),
    }
}

/// Held for the duration of an interactive login. The advisory lock is
/// released when the file closes on drop — including on panic — so a crashed
/// pane cannot wedge the others out, which a pid-file scheme could not promise.
///
/// `Debug` is derived and prints nothing sensitive: `std::fs::File`'s own
/// `Debug` shows a descriptor and path, never contents.
#[derive(Debug)]
pub struct LoginLock(#[expect(dead_code)] std::fs::File);

/// Why an interactive login could not take the cross-pane lock.
///
/// The two are not interchangeable, and collapsing them was a dead-end
/// diagnosis: "another pane is already running a login" sends the user to look
/// for a login nobody started.
#[derive(Debug)]
pub enum LockDenied {
    /// Another murmur pane holds the lock. Real contention; wait it out.
    Busy,
    /// The lock could not be taken at all — a read-only `~/.mur`, a
    /// filesystem with no advisory locking. Nothing is holding anything.
    Unavailable(std::io::Error),
}

/// Take the cross-pane login lock.
///
/// `fs2`, not `libc::flock`: this crate runs on Windows CI (two matrices in
/// `.github/workflows/ci.yml`), `fs2` is already a `mur-core` dependency, and
/// `fs2::FileExt` is already how this crate locks files — see
/// `inject/queue.rs`, `cmd/agent_companion/init.rs`, and
/// `cross_agent/propagate/mod.rs`.
///
/// Contention is told apart from failure by the OS error code `fs2` itself
/// uses for a contended lock (`fs2::lock_contended_error`), rather than by
/// `io::ErrorKind`, which maps differently per platform.
pub fn acquire_login_lock(home: &Path) -> Result<LoginLock, LockDenied> {
    use fs2::FileExt;
    let path = home.join("login.lock");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(LockDenied::Unavailable)?;
    match f.try_lock_exclusive() {
        Ok(()) => Ok(LoginLock(f)),
        Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            Err(LockDenied::Busy)
        }
        Err(e) => Err(LockDenied::Unavailable(e)),
    }
}

/// Rung 3. Only reached when nothing cheaper worked.
fn request_login_handover(app: &mut crate::cmd::agent::cli::app::App, p: Provider) {
    if !has_browser(&BrowserEnv::detect()) {
        app.push_system(format!("{}:\n{}", p.label(), print_only_instructions(p)));
        return;
    }
    let lock = match acquire_login_lock(&app.home) {
        Ok(l) => Some(l),
        Err(LockDenied::Busy) => {
            app.push_error(
                "another murmur pane is already running a login — finish that one first",
            );
            return;
        }
        Err(LockDenied::Unavailable(e)) => {
            // Nothing is holding anything, so refusing would be a dead end of
            // its own — on a filesystem without advisory locking the user
            // could never log in from murmur at all. The lock only serialises
            // panes; losing it degrades to the behaviour that shipped before
            // it existed, so warn and continue.
            app.push_warn(format!(
                "login lock unavailable ({e}) — continuing without it; \
                 don't start a second login in another pane"
            ));
            None
        }
    };
    let argv = match p {
        Provider::Anthropic => vec![p.owner_cli().into(), "auth".into(), "login".into()],
        Provider::Chatgpt => vec![p.owner_cli().into(), "login".into()],
    };
    app.pending_handover = Some(crate::cmd::agent::cli::app::HandoverRequest {
        argv,
        label: p.label().to_string(),
        _lock: lock,
    });
}

#[cfg(test)]
#[path = "login/tests.rs"]
mod tests;

#[cfg(test)]
mod endpoint_line_tests {
    use super::*;

    /// A status line must not resolve the credential: on macOS a `keychain:`
    /// ref pops an authorization prompt, and typing `/login` to look at a table
    /// is not asking for one. `mur agent doctor` resolves deliberately, because
    /// a human asked it to check.
    #[test]
    fn the_endpoint_line_names_the_credential_without_resolving_it() {
        let src = include_str!("login.rs");
        let body = src
            .split("fn agent_endpoint_line")
            .nth(1)
            .expect("function must exist");
        let body = &body[..body.find("\n}\n").unwrap_or(body.len())];
        assert!(
            !body.contains("resolve_blocking"),
            "a status line must not resolve a secret"
        );
    }

    /// An unknown agent must produce nothing rather than an error banner: the
    /// OAuth table above is still useful and this line is an addition to it.
    #[test]
    fn an_unknown_agent_adds_nothing() {
        assert_eq!(agent_endpoint_line("no-such-agent-xyz"), "");
    }
}
