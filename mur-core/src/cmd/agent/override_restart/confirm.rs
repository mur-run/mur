//! The human step between `prepare` and `override_restart` (spec
//! §Confirmation): a banner and a typed agent name on a TTY, `--confirm`
//! otherwise. Nothing here writes the override file.

use std::io::{BufRead, Write};

use mur_common::agent::OverrideFlags;
use mur_common::startup_override::ConfirmedBy;

/// What the human answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::cmd::agent) enum Confirmation {
    Confirmed(ConfirmedBy),
    /// The typed name did not match (or input ended). Audit as declined.
    Declined,
    /// Not a TTY and no `--confirm`: refuse before anything is written.
    NeedsConfirmFlag,
}

/// The terminal, injected so tests drive both branches.
pub(in crate::cmd::agent) struct Terminal<'a> {
    pub is_tty: bool,
    pub input: &'a mut dyn BufRead,
    pub output: &'a mut dyn Write,
}

pub(in crate::cmd::agent) fn confirm(
    name: &str,
    flags: OverrideFlags,
    flag_confirm: bool,
    term: Terminal<'_>,
) -> Confirmation {
    if !term.is_tty {
        return if flag_confirm {
            Confirmation::Confirmed(ConfirmedBy::FlagConfirm)
        } else {
            Confirmation::NeedsConfirmFlag
        };
    }
    // A TTY always asks: `--confirm` is the non-TTY answer, not a skip.
    let _ = write!(term.output, "{}", banner(name, flags));
    let _ = write!(term.output, "Type the agent name ({name}) to continue: ");
    let _ = term.output.flush();
    let mut typed = String::new();
    match term.input.read_line(&mut typed) {
        Ok(n) if n > 0 && typed.trim() == name => Confirmation::Confirmed(ConfirmedBy::Tty),
        _ => Confirmation::Declined,
    }
}

/// What each flag relaxes and what stays on, for the next boot only.
pub(in crate::cmd::agent) fn banner(name: &str, flags: OverrideFlags) -> String {
    let mut s = format!(
        "\n⚠  Startup override for '{name}' — the NEXT boot only, then normal permissions.\n\n"
    );
    if flags.no_hitl {
        s.push_str(
            "  --no-hitl: calls that would ask for approval run without asking.\n\
             \x20   Still ask: calls classified Spend or Privileged, and calls with no\n\
             \x20   declared tier (declare a `risk:` tier to change that). Explicit per-tool\n\
             \x20   `ask` rules, `deny` rules and remembered denials still apply.\n\
             \x20   Unattended, a call that still has to ask is deferred — held until a\n\
             \x20   human answers — never released on its own.\n",
        );
    }
    if flags.net_unrestricted {
        s.push_str(
            "  --net=unrestricted: shell and child-process network egress is open.\n\
             \x20   Still on: MCP servers stay behind the egress proxy, and its credential\n\
             \x20   scan.\n",
        );
    }
    if flags.full_perms {
        s.push_str(
            "  --full-perms: filesystem deny list reset to the defaults (~/.ssh, ~/.aws,\n\
             \x20   ~/.gnupg) and any process may be spawned.\n\
             \x20   Still on: credential paths stay unreadable, protected paths (incl.\n\
             \x20   secrets/) stay unwritable, and a sandbox error still fails closed.\n",
        );
    }
    s.push('\n');
    s
}

#[cfg(test)]
#[path = "confirm_tests.rs"]
mod tests;
