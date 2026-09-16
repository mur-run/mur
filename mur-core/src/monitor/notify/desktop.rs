//! Desktop notification, following `cmd/agent/cli/notify.rs`'s established
//! shape: a pure script builder so the escaping is unit-tested, and a
//! spawn-and-ignore delivery. Opt-in (`notifications.desktop`, default
//! false): a background daemon that starts popping OS notifications the
//! moment a user upgrades is a hostile default.

use mur_monitor::notify::Notification;

use super::Channel;

/// Inserted for each newline in the message so a multi-line `render()`
/// body still ends up as a single osascript line. Named rather than
/// inlined so the ordering test below can substitute a value that would
/// actually be dangerous under the old ordering, without ever putting a
/// dangerous value on the real code path.
const NEWLINE_PLACEHOLDER: &str = " · ";

/// Escapes `s` for embedding inside an osascript double-quoted literal,
/// with `placeholder` standing in for every newline.
///
/// Order matters: the placeholder is substituted FIRST, then backslashes
/// are escaped, then quotes. That way, whatever `placeholder` contains
/// gets escaped along with the rest of the string instead of being
/// spliced into the line raw. This used to escape-then-substitute, which
/// looks equivalent for today's harmless `" · "` but is a landmine for
/// whoever next changes that text to something containing a quote or a
/// backslash — under the old order that content would reach the
/// osascript line unescaped. Backslashes are escaped before quotes so
/// quote-escaping's own inserted backslash isn't re-escaped.
fn esc_with_placeholder(s: &str, placeholder: &str) -> String {
    s.replace('\n', placeholder)
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn esc(s: &str) -> String {
    esc_with_placeholder(s, NEWLINE_PLACEHOLDER)
}

/// One osascript line, with quotes, backslashes and newlines escaped.
/// Newlines matter: `render`'s body is multi-line, and an unescaped one
/// truncates the script — delivering half a message with no error.
pub fn script(title: &str, message: &str) -> String {
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
        let body = format!("{} — {}", n.next_step, n.body).replace('\n', NEWLINE_PLACEHOLDER);
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

    #[test]
    fn the_newline_placeholders_own_content_is_escaped_not_injected_raw() {
        // `NEWLINE_PLACEHOLDER` (" · ") is harmless today, so this scenario
        // cannot be reproduced through `script()`'s public surface as it
        // stands — that gap is exactly what this test closes. It feeds
        // `esc_with_placeholder` a placeholder that DOES contain a quote,
        // proving the substitute-then-escape order catches it, and
        // reproduces the old escape-then-substitute order inline (never
        // exported — it exists only to characterize the bug being fixed)
        // to show that order would have let the quote through raw.
        let hostile_placeholder = "\"";
        let body = "line one\nline two";

        let new_order = esc_with_placeholder(body, hostile_placeholder);
        assert_eq!(
            new_order, "line one\\\"line two",
            "the placeholder's quote must come out escaped: {new_order:?}"
        );

        let old_order = body
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', hostile_placeholder);
        assert_eq!(
            old_order, "line one\"line two",
            "old ordering should leak the placeholder's quote unescaped \
             (that's the bug this fix removes): {old_order:?}"
        );
    }
}
