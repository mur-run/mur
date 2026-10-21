//! `mur agent status` for an agent whose runtime refused to start (#2015).
//!
//! The runtime records the refusal in `<agent_home>/start_refused.json` and
//! the service manager stops respawning it, so `status` is where the reader
//! finds out why the agent is down — without opening a log.

use std::path::Path;

use mur_common::start_refusal::{self, StartRefusal};

/// The refusal to show for an agent that is not running, or `None`.
///
/// A running agent never shows one: the runtime clears the marker once it is
/// past its refusal checks, so a marker beside a live lock can only be a race
/// with that clear, and must not make a healthy agent look refused.
pub(super) fn refusal_for(agent_home: &Path, running: bool) -> Option<StartRefusal> {
    if running {
        return None;
    }
    start_refusal::read(agent_home)
}

/// The lines `status` prints for `refusal`, indented to sit under `Active:`.
/// The runtime's own message already names the remedy; an entitlement refusal
/// gets the reseal command spelled out on its own line as well, so it is
/// copyable even when the message wraps.
pub(super) fn refusal_lines(name: &str, refusal: &StartRefusal) -> Vec<String> {
    let mut lines = vec![
        format!(
            "   Refused: {} — it will not be restarted automatically",
            refusal.code
        ),
        format!("     Reason: {}", refusal.message),
    ];
    if refusal.is_entitlements() {
        lines.push(format!(
            "     Fix: review the change, then `mur agent perm reseal {name}` and `mur agent start {name}`"
        ));
    } else {
        lines.push(format!(
            "     Fix: correct {}, then `mur agent start {name}`",
            remedy_target(&refusal.code)
        ));
    }
    lines
}

fn remedy_target(code: &str) -> &'static str {
    match code {
        "profile_invalid" => "profile.yaml",
        _ => "the cause above",
    }
}

/// `status` for an agent directory whose profile cannot be loaded, so it is
/// absent from the agent table. Without this, a runtime that refused on a
/// malformed `profile.yaml` read as "agent not found" — the one place the
/// reader went to ask why it was down said it did not exist.
///
/// Returns `None` when there is no recorded refusal; the caller keeps its
/// "not found" error for that case.
///
/// `name` comes straight from the command line and is joined onto a path
/// here, so it is validated first: `status ../x` must stay "not found".
pub(super) fn unloadable_status(name: &str, agents_dir: &Path) -> Option<Vec<String>> {
    mur_common::agent_name::validate_agent_name(name).ok()?;
    let agent_home = agents_dir.join(name);
    if !agent_home.is_dir() {
        return None;
    }
    let refusal = start_refusal::read(&agent_home)?;
    let mut lines = vec![
        format!("● {name} - (profile not loadable)"),
        format!("   Loaded: {}", agent_home.display()),
        "   Active: refused".to_string(),
    ];
    lines.extend(refusal_lines(name, &refusal));
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::start_refusal::write;

    fn refusal(line: &str) -> StartRefusal {
        StartRefusal::from_line(line, 1)
    }

    #[test]
    fn entitlement_refusal_names_the_reseal_command() {
        let r = refusal("error[entitlements_unpinned]: bob's profile.yaml entitlements changed");
        let text = refusal_lines("bob", &r).join("\n");
        assert!(text.contains("entitlements_unpinned"), "{text}");
        assert!(text.contains("entitlements changed"), "{text}");
        assert!(text.contains("mur agent perm reseal bob"), "{text}");
        assert!(text.contains("not be restarted automatically"), "{text}");
    }

    #[test]
    fn invalid_profile_refusal_points_at_the_profile_not_reseal() {
        let r = refusal("error[profile_invalid]: missing field `schema`");
        let text = refusal_lines("eve", &r).join("\n");
        assert!(text.contains("missing field `schema`"), "{text}");
        assert!(text.contains("profile.yaml"), "{text}");
        assert!(!text.contains("reseal"), "{text}");
    }

    #[test]
    fn a_running_agent_never_shows_a_refusal() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), &refusal("error[entitlements_pin]: x")).unwrap();
        assert!(refusal_for(tmp.path(), true).is_none());
        assert!(refusal_for(tmp.path(), false).is_some());
    }

    #[test]
    fn unloadable_without_a_marker_stays_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let eve = tmp.path().join("eve");
        assert!(unloadable_status("eve", tmp.path()).is_none(), "no dir");
        std::fs::create_dir_all(&eve).unwrap();
        assert!(unloadable_status("eve", tmp.path()).is_none(), "no marker");
        write(&eve, &refusal("error[profile_invalid]: bad")).unwrap();
        // A traversal name must not reach a marker outside `agents/`.
        write(tmp.path(), &refusal("error[profile_invalid]: outside")).unwrap();
        assert!(unloadable_status("..", &eve).is_none());
        let text = unloadable_status("eve", tmp.path()).unwrap().join("\n");
        assert!(text.contains("Active: refused"), "{text}");
        assert!(text.contains("error[profile_invalid]: bad"), "{text}");
    }
}
