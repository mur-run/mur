//! What `/channels` prints: the conversations a pane can switch to, the runs
//! it can only follow, and the refusal when a run is named without `--follow`.

use super::{
    CHANNEL_LOOKUP_LIMIT, ChannelRef, FIRST_ORDINAL, NO_ORDINAL, channel_line, persist, resolve,
};
use std::path::Path;

/// One row of the run section of the `/channels` listing. Counts events, not
/// turns: nobody takes turns in a run.
fn run_line(s: &persist::SessionInfo) -> String {
    let n = if s.ordinal >= FIRST_ORDINAL {
        s.ordinal.to_string()
    } else {
        NO_ORDINAL.to_string()
    };
    format!(
        "  {} · {} · {} events · {}\n",
        n, s.handle, s.turns, s.preview
    )
}

/// The `/channels` listing: the conversations this pane can switch to, then
/// the runs it can only follow. `None` when there is nothing to list.
///
/// Runs get their own section rather than sharing the rows above, so the
/// header over a row always says what its number does: `/channels N` on a run
/// is refused (#2515), `/channels N --follow` is not, and the number has to be
/// readable somewhere for that second form to be usable.
pub(in crate::cmd::agent::cli) fn listing(
    conversations: &[persist::SessionInfo],
    runs: &[persist::SessionInfo],
) -> Option<String> {
    if conversations.is_empty() && runs.is_empty() {
        return None;
    }
    let mut out = String::new();
    if !conversations.is_empty() {
        out.push_str("channels (/channels N or id-prefix to switch · add --follow to tail):\n");
        for s in conversations {
            out.push_str(&channel_line(s));
        }
    }
    if !runs.is_empty() {
        out.push_str("runs (follow only — /channels N --follow):\n");
        for s in runs {
            out.push_str(&run_line(s));
        }
    }
    Some(out.trim_end().to_string())
}

/// What the user typed, in a form that parses back to the same target.
fn typed(target: &ChannelRef) -> String {
    match target {
        ChannelRef::Ordinal(n) => n.to_string(),
        ChannelRef::IdPrefix(p) => format!("#{p}"),
        ChannelRef::Malformed(w) => w.clone(),
    }
}

/// The refusal for `/channels <target>` when `target` names a run: say what
/// it is and give the command that works. `None` when it names no run, so the
/// caller's ordinary "no channel" message stands.
///
/// Without this a number read off the run section answers "no channel N —
/// /channels to list", and the listing it points at shows channel N.
pub(in crate::cmd::agent::cli) fn follow_only_msg(
    home: &Path,
    agent: &str,
    target: &ChannelRef,
) -> Option<String> {
    let runs = persist::list_runs(home, agent, CHANNEL_LOOKUP_LIMIT).ok()?;
    resolve(&runs, target).ok()?;
    let t = typed(target);
    Some(format!(
        "{t} is a fleet or workflow run — follow it, a pane cannot switch to one: /channels {t} --follow"
    ))
}
