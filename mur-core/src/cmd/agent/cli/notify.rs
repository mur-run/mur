//! Desktop notification for an unfocused terminal, moved out of `mod.rs` for CLAUDE.md §4's
//! 800-line rule. Pure movement: verbatim.

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

/// Best-effort OS notification — fired only when the terminal is unfocused.
/// Never blocks or errors the event loop (spawn-and-ignore), and emits no
/// in-terminal bell (the TUI owns the alternate screen).
pub(super) fn notify_unfocused(title: &str, message: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("osascript")
            .args(["-e", &notify_script(title, message)])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
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
