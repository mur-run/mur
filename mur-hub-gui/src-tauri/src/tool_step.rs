//! Which tool steps of one streamed turn finished: the Hub's trigger for a
//! card that a tool call wrote a request for (SSH, browser).

use std::collections::HashSet;

use mur_core::a2a_dial::StepEvent;

/// Track `tool`'s steps of one streamed turn; true once when one finishes.
pub fn is_tool_done(seen: &mut HashSet<String>, step: &StepEvent, tool: &str) -> bool {
    match step {
        StepEvent::Started { name, step_id, .. } if name == tool => {
            seen.insert(step_id.clone());
            false
        }
        StepEvent::Completed { step_id, .. } => seen.remove(step_id),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(id: &str, name: &str) -> StepEvent {
        StepEvent::Started {
            step_id: id.into(),
            task_id: "t".into(),
            name: name.into(),
            args: serde_json::Value::Null,
        }
    }

    fn completed(id: &str) -> StepEvent {
        mur_core::a2a_dial::parse_step(&serde_json::json!({ "step_id": id }), true)
    }

    #[test]
    fn only_a_finished_step_of_that_tool_counts_once() {
        let mut seen = HashSet::new();
        assert!(!is_tool_done(
            &mut seen,
            &started("1", "bash"),
            "ssh_request"
        ));
        assert!(!is_tool_done(&mut seen, &completed("1"), "ssh_request"));
        assert!(!is_tool_done(
            &mut seen,
            &started("2", "ssh_request"),
            "ssh_request"
        ));
        assert!(is_tool_done(&mut seen, &completed("2"), "ssh_request"));
        assert!(
            !is_tool_done(&mut seen, &completed("2"), "ssh_request"),
            "once"
        );
    }

    #[test]
    fn each_tool_keeps_its_own_steps() {
        let (mut ssh, mut web) = (HashSet::new(), HashSet::new());
        let s = started("3", "browser_request");
        assert!(!is_tool_done(&mut ssh, &s, "ssh_request"));
        assert!(!is_tool_done(&mut web, &s, "browser_request"));
        assert!(!is_tool_done(&mut ssh, &completed("3"), "ssh_request"));
        assert!(is_tool_done(&mut web, &completed("3"), "browser_request"));
    }
}
