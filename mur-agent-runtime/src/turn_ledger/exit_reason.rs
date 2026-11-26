//! The reason line for a command that ran and exited non-zero.

/// `exit N: <last line the command printed>`. The `[exit code: N]` marker and
/// `[stderr]` separator are bash's framing, not the command's words, so they
/// are skipped; the last real line is usually the one that says why.
pub(super) fn exit_reason(content: &str, exit_code: i32) -> String {
    let last = content
        .lines()
        .map(str::trim)
        .rev()
        .find(|l| !l.is_empty() && !l.starts_with("[exit code:") && *l != "[stderr]");
    match last {
        Some(l) => format!("exit {exit_code}: {l}"),
        None => format!("exit {exit_code}"),
    }
}
