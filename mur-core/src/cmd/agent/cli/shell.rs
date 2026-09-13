//! Local `!command` execution for the murmur TUI: spawning, streaming,
//! cancelling, and deciding where a finished command's output goes.
//!
//! Separate from `stream.rs`, which is the A2A streaming bridge and has
//! nothing to do with local processes, and separate from `mod.rs`, which is
//! already far past the repository's 800-line rule (CLAUDE.md §4).

/// What the agent receives for a `!cmd` run. Singular, framed, nothing else:
/// the agent may answer with one line, and the block does not ask for more.
pub(super) fn shell_block(cmd: &str, output: &str) -> String {
    if output.is_empty() {
        format!("[shell command the user ran locally]\n$ {cmd}\n[end of shell output]")
    } else {
        format!("[shell command the user ran locally]\n$ {cmd}\n{output}\n[end of shell output]")
    }
}

/// Where a finished `!cmd` block goes.
#[derive(Debug)]
pub(super) enum ShellRoute {
    /// Idle: start a turn with the block as the user's message.
    Start,
    /// A turn is live: steer it with the block.
    Steer(String),
    /// Nowhere; the note says why. The Shell card still renders.
    Skip(&'static str),
}

/// Pure so the three routes are testable without pricing or a live agent.
/// The budget gates a NEW turn only, exactly as `submit` does for typed text:
/// a steer rides the turn already being paid for.
pub(super) fn route_shell_output(
    streaming: bool,
    task_id: Option<&str>,
    over_budget: bool,
) -> ShellRoute {
    if streaming {
        return match task_id {
            Some(t) => ShellRoute::Steer(t.to_string()),
            None => {
                ShellRoute::Skip("shell output not sent — a turn is generating without a task id")
            }
        };
    }
    if over_budget {
        return ShellRoute::Skip("↯ shell output not sent — session budget reached");
    }
    ShellRoute::Start
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_output_routes_by_turn_state() {
        assert!(matches!(
            route_shell_output(false, None, false),
            ShellRoute::Start
        ));
        assert!(
            matches!(route_shell_output(true, Some("t1"), false), ShellRoute::Steer(ref t) if t == "t1")
        );
        assert!(matches!(
            route_shell_output(true, None, false),
            ShellRoute::Skip(_)
        ));
        // Budget gates a NEW turn only; a steer rides the turn already paid for.
        assert!(matches!(
            route_shell_output(false, None, true),
            ShellRoute::Skip(_)
        ));
        assert!(matches!(
            route_shell_output(true, Some("t1"), true),
            ShellRoute::Steer(_)
        ));
    }

    #[test]
    fn shell_block_frames_command_and_output() {
        assert_eq!(
            shell_block("ls", "a\nb"),
            "[shell command the user ran locally]\n$ ls\na\nb\n[end of shell output]"
        );
        assert_eq!(
            shell_block("true", ""),
            "[shell command the user ran locally]\n$ true\n[end of shell output]"
        );
    }
}
