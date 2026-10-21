//! Heredoc bodies that are data, not commands, removed before tier scanning.
//!
//! `tool_tier` matches deny-listed words anywhere in a bash command. A file
//! written with `cat > f <<'EOF' … EOF` whose source merely *mentions*
//! `launchctl` or `chmod` was therefore tiered Privileged (seen live while an
//! agent wrote a launchd fix, #2015). The body of a heredoc fed to a plain
//! data sink is never executed, so it is dropped from the scanned text.
//!
//! Conservative by construction — a body is dropped only when ALL of:
//! - its `<<` is unquoted (a `<<` inside quotes is literal text);
//! - the command that owns it is a known data sink (`cat`, `tee`);
//! - nothing on that line pipes it onward (`cat <<EOF | sh` executes it);
//! - its terminator line is actually present.
//!
//! Anything else keeps the body, so this can only ever LOWER a tier for the
//! exact shape it understands, and the original command still goes to
//! `opaque_shell` untouched.

use std::borrow::Cow;

/// Commands that only copy their stdin somewhere; a heredoc fed to one is data.
const DATA_SINKS: &[&str] = &["cat", "tee"];

struct Pending {
    delim: String,
    sink: bool,
}

/// `cmd` with the bodies of data-sink heredocs removed (see module docs).
pub(super) fn scan_text(cmd: &str) -> Cow<'_, str> {
    if !cmd.contains("<<") {
        return Cow::Borrowed(cmd);
    }
    let mut out = String::with_capacity(cmd.len());
    let mut seg_start = 0;
    let mut quote: Option<char> = None;
    let mut pending: Vec<Pending> = Vec::new();
    let mut rest = cmd;

    while let Some(c) = rest.chars().next() {
        rest = &rest[c.len_utf8()..];
        if let Some(q) = quote {
            out.push(c);
            if c == '\\' && q == '"' {
                if let Some(n) = rest.chars().next() {
                    out.push(n);
                    rest = &rest[n.len_utf8()..];
                }
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                out.push(c);
            }
            '\\' => {
                out.push(c);
                if let Some(n) = rest.chars().next() {
                    out.push(n);
                    rest = &rest[n.len_utf8()..];
                }
            }
            '<' if rest.starts_with('<') => {
                rest = &rest[1..];
                out.push_str("<<");
                if rest.starts_with('<') {
                    // `<<<` here-string: its word stays on this line.
                    rest = &rest[1..];
                    out.push('<');
                } else {
                    let (delim, raw_len) = delimiter(rest);
                    out.push_str(&rest[..raw_len]);
                    rest = &rest[raw_len..];
                    if let Some(delim) = delim {
                        let sink = segment_head(&out[seg_start..])
                            .is_some_and(|h| DATA_SINKS.contains(&h));
                        pending.push(Pending { delim, sink });
                    }
                }
            }
            ';' | '&' | '|' | '\n' => {
                out.push(c);
                if c == '|' && !rest.starts_with('|') {
                    // Piped onward: whatever reads it may execute it.
                    pending.iter_mut().for_each(|p| p.sink = false);
                }
                if c == '\n' && !pending.is_empty() {
                    rest = take_bodies(rest, std::mem::take(&mut pending), &mut out);
                }
                seg_start = out.len();
            }
            _ => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// The delimiter word after `<<` (`-`, quotes and backslashes removed), and
/// how many bytes of `rest` it spans.
fn delimiter(rest: &str) -> (Option<String>, usize) {
    let mut delim = String::new();
    let mut len = 0;
    let mut started = false;
    for c in rest.chars() {
        if !started && (c == '-' && len == 0 || c == ' ' || c == '\t') {
            len += c.len_utf8();
            continue;
        }
        if c.is_whitespace() || matches!(c, ';' | '&' | '|' | '<' | '>' | '(' | ')') {
            break;
        }
        started = true;
        len += c.len_utf8();
        if !matches!(c, '\'' | '"' | '\\') {
            delim.push(c);
        }
    }
    ((!delim.is_empty()).then_some(delim), len)
}

/// First word of the segment being built, by basename (`/bin/cat` → `cat`).
fn segment_head(seg: &str) -> Option<&str> {
    let w = seg.split_whitespace().next()?;
    w.rsplit('/').next()
}

/// Consume each pending body up to its terminator line. A data-sink body with
/// a terminator is dropped; any other body (or an unterminated one) is copied
/// through so the tier scan still sees it.
fn take_bodies<'a>(mut rest: &'a str, pending: Vec<Pending>, out: &mut String) -> &'a str {
    for p in pending {
        let mut end = None;
        let mut pos = 0;
        while pos < rest.len() {
            let line_end = rest[pos..].find('\n').map_or(rest.len(), |i| pos + i);
            let line = &rest[pos..line_end];
            let next = (line_end + 1).min(rest.len());
            if line.trim() == p.delim {
                end = Some((pos, next));
                break;
            }
            pos = next;
        }
        match end {
            Some((term_start, next)) if p.sink => {
                out.push_str(&rest[term_start..next]);
                rest = &rest[next..];
            }
            Some((_, next)) => {
                out.push_str(&rest[..next]);
                rest = &rest[next..];
            }
            None => {
                out.push_str(rest);
                return "";
            }
        }
    }
    rest
}

#[cfg(test)]
mod tests {
    use super::scan_text;

    #[test]
    fn a_sink_body_is_dropped_but_the_command_line_stays() {
        let s = scan_text("cd /r && cat > f.rs <<'EOF'\nlaunchctl x\nEOF\nsudo ls");
        assert!(!s.contains("launchctl"), "{s}");
        assert!(s.contains("cat > f.rs") && s.contains("sudo ls"), "{s}");
    }

    #[test]
    fn bodies_that_may_execute_are_kept() {
        for cmd in [
            "bash <<'EOF'\nlaunchctl x\nEOF",
            "cat <<EOF | sh\nlaunchctl x\nEOF",
            "python3 - <<EOF\nlaunchctl x\nEOF",
            // No terminator: cannot tell where the body ends.
            "cat > f <<EOF\nlaunchctl x",
            // `<<` inside quotes is literal; the next line is a real command.
            "echo \"; cat <<X\"\nlaunchctl x\nX",
        ] {
            assert!(scan_text(cmd).contains("launchctl"), "cmd: {cmd}");
        }
    }

    #[test]
    fn here_strings_and_tee_are_handled() {
        assert_eq!(scan_text("cat <<< \"$X\""), "cat <<< \"$X\"");
        let s = scan_text("tee a.txt <<-EOF >/dev/null\n\tchmod 777 x\n\tEOF");
        assert!(!s.contains("chmod"), "{s}");
    }

    /// End to end through the classifier: source that merely mentions a
    /// privileged word, written to a file, must not tier the write as
    /// Privileged. Seen live: an agent writing a launchd fix (#2015).
    #[test]
    fn a_heredoc_written_to_a_file_is_not_a_command() {
        let cmd = "cd /repo && cat > src/refusal.rs <<'EOF'\n\
                   //! The plist is KeepAlive, so launchctl respawns it.\n\
                   //! Do not chmod the socket.\n\
                   EOF";
        let t =
            crate::hitl::tool_tier::classify("bash", Some(&serde_json::json!({ "command": cmd })));
        assert!(crate::hitl::tier_may_be_granted(t), "escalated to {t:?}");
    }

    /// The other half: a heredoc fed to a shell IS executed and keeps its tier.
    #[test]
    fn a_heredoc_fed_to_a_shell_keeps_its_tier() {
        let cmd = "bash <<'EOF'\nlaunchctl bootout gui/501/x\nEOF";
        let t =
            crate::hitl::tool_tier::classify("bash", Some(&serde_json::json!({ "command": cmd })));
        assert!(!crate::hitl::tier_may_be_granted(t), "granted at {t:?}");
    }
}
