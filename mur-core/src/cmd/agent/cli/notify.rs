//! Desktop notification for an unfocused terminal, moved out of `mod.rs` for CLAUDE.md §4's
//! 800-line rule.

/// Terminals we can name a bundle id for, keyed by the `TERM_PROGRAM` value
/// each one exports. Only consulted when `__CFBundleIdentifier` is absent.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const TERM_PROGRAM_BUNDLE_IDS: &[(&str, &str)] = &[
    ("ghostty", "com.mitchellh.ghostty"),
    ("iTerm.app", "com.googlecode.iterm2"),
    ("Apple_Terminal", "com.apple.Terminal"),
    ("WezTerm", "com.github.wez.wezterm"),
    ("alacritty", "org.alacritty"),
    ("kitty", "net.kovidgoyal.kitty"),
    ("vscode", "com.microsoft.VSCode"),
    ("Hyper", "co.zeit.hyper"),
    ("Warp", "dev.warp.Warp-Stable"),
];

/// The helper that can attribute a notification to another app; absent on most
/// machines, in which case we fall back to `osascript`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const NOTIFIER_BIN: &str = "terminal-notifier";

/// Env var macOS sets on processes launched from a bundle — the terminal's own
/// bundle id, and the most reliable source when it is present.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const BUNDLE_ID_ENV: &str = "__CFBundleIdentifier";

/// Env var terminals use to announce themselves.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const TERM_PROGRAM_ENV: &str = "TERM_PROGRAM";

/// Resolve a `TERM_PROGRAM` value to a bundle id. Pure so the table is testable
/// without a terminal; matching is case-insensitive because the casing varies.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn bundle_id_for_term_program(term_program: &str) -> Option<&'static str> {
    TERM_PROGRAM_BUNDLE_IDS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(term_program))
        .map(|(_, id)| *id)
}

/// The host terminal's bundle id, or `None` when it cannot be established.
/// `None` means "notify without a click target" — never a guess, since a wrong
/// id sends the click to the wrong app (that is the Script Editor bug).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn terminal_bundle_id() -> Option<String> {
    if let Ok(id) = std::env::var(BUNDLE_ID_ENV)
        && !id.is_empty()
    {
        return Some(id);
    }
    let term_program = std::env::var(TERM_PROGRAM_ENV).ok()?;
    bundle_id_for_term_program(&term_program).map(str::to_owned)
}

/// The macOS `osascript` line for a notification, with quotes escaped. Pure so
/// the escaping is unit-tested; the spawn is in `notify_unfocused`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn notify_script(title: &str, message: &str) -> String {
    format!(
        "display notification \"{}\" with title \"{}\"",
        message.replace('\\', "\\\\").replace('"', "\\\""),
        title.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

/// `terminal-notifier` parses its argv as `-key value` pairs, so a value that
/// itself starts with `-` is read as the next key and the text is lost. A
/// leading space is invisible in the banner and ends that ambiguity.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn sanitize_notifier_arg(value: &str) -> String {
    if value.starts_with('-') {
        format!(" {value}")
    } else {
        value.to_owned()
    }
}

/// Post via `terminal-notifier`, attributing the notification to the host
/// terminal so clicking it raises that terminal. Waits for the exit status:
/// the helper can be present but unable to run (a wrapper script whose exec
/// fails), and a spawn that succeeds would otherwise swallow the notification.
/// Any non-success is the signal to fall back.
#[cfg(target_os = "macos")]
fn notify_via_terminal_notifier(
    title: &str,
    message: &str,
    bundle_id: &str,
) -> std::io::Result<std::process::ExitStatus> {
    std::process::Command::new(NOTIFIER_BIN)
        .args([
            "-title",
            &sanitize_notifier_arg(title),
            "-message",
            &sanitize_notifier_arg(message),
            "-sender",
            bundle_id,
            "-activate",
            bundle_id,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
}

/// Best-effort OS notification — fired only when the terminal is unfocused.
/// Never blocks or errors the event loop (spawn-and-ignore), and emits no
/// in-terminal bell (the TUI owns the alternate screen).
pub(super) fn notify_unfocused(title: &str, message: &str) {
    #[cfg(target_os = "macos")]
    {
        // `status()` waits, so the whole attempt runs off the event loop.
        let (title, message) = (title.to_owned(), message.to_owned());
        std::thread::spawn(move || {
            // Preferred path: click returns to the terminal that started us.
            // Success is the exit code, not the spawn — the helper may be
            // installed yet unable to run, and then nothing was posted.
            if let Some(bundle_id) = terminal_bundle_id()
                && let Ok(status) = notify_via_terminal_notifier(&title, &message, &bundle_id)
                && status.success()
            {
                return;
            }
            // Fallback: `osascript` owns the notification, so it is posted
            // without an activation target rather than pointed at the wrong app.
            let _ = std::process::Command::new("osascript")
                .args(["-e", &notify_script(&title, &message)])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        });
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .args([title, message])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (title, message); // no-op on other platforms
    }
}
