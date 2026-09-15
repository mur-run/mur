//! Desktop notification, following `cmd/agent/cli/notify.rs`'s established
//! shape: a pure script builder so the escaping is unit-tested, and a
//! spawn-and-ignore delivery. Opt-in (`notifications.desktop`, default
//! false): a background daemon that starts popping OS notifications the
//! moment a user upgrades is a hostile default.

use mur_monitor::notify::Notification;

use super::Channel;

/// One osascript line, with quotes, backslashes and newlines escaped.
/// Newlines matter: `render`'s body is multi-line, and an unescaped one
/// truncates the script — delivering half a message with no error.
pub fn script(title: &str, message: &str) -> String {
    fn esc(s: &str) -> String {
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', " · ")
    }
    format!(
        "display notification \"{}\" with title \"{}\"",
        esc(message),
        esc(title)
    )
}

pub struct DesktopChannel;

impl Channel for DesktopChannel {
    fn name(&self) -> &'static str {
        "desktop"
    }

    fn deliver(&self, n: &Notification) -> Result<(), String> {
        // Lead with the next step: a banner truncates, and the step is the
        // part that is worth the interruption.
        let body = format!("{} — {}", n.next_step, n.body.replace('\n', " · "));
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("osascript")
                .args(["-e", &script(&n.title, &body)])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("osascript: {e}"))
        }
        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("notify-send")
                .args([&n.title, &body])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("notify-send: {e}"))
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = body;
            Err("no desktop notification backend on this platform".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_and_backslashes_are_escaped_for_osascript() {
        let s = script("MUR monitor: a\"b", "c\\d");
        assert!(s.contains("a\\\"b"), "{s}");
        assert!(s.contains("c\\\\d"), "{s}");
    }

    #[test]
    fn a_newline_in_the_body_does_not_break_the_script() {
        // `render` puts newlines in `body`; an unescaped one would truncate
        // the osascript line and silently deliver half a message.
        let s = script("t", "line one\nline two");
        assert!(!s.contains('\n'), "the script must be a single line: {s:?}");
        assert!(s.contains("line one") && s.contains("line two"), "{s}");
    }

    #[test]
    fn the_channel_is_named_desktop() {
        assert_eq!(DesktopChannel.name(), "desktop");
    }
}
