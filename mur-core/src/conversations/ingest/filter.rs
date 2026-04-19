//! Stage 3: conservative REJECT gate. Only drops provable noise.
//! Spec §5.4; research §5 (Mem0 97.8% junk audit — the lesson is "when in
//! doubt, accept": bias toward preserving rather than over-filtering).

use mur_common::{Content, Message, Role};

#[derive(Debug)]
pub enum Decision {
    Accept,
    Reject(&'static str),
}

const HEARTBEAT: &[&str] = &[
    "[heartbeat]",
    "heartbeat",
    "ping",
    "pong",
    "health check",
    "liveness probe",
];
const RESTATEMENT: &[&str] = &[
    "you are claude",
    "you are an ai",
    "i am claude",
    "i'm claude",
    "created by anthropic",
    "helpful, harmless, and honest",
];

pub fn decide(msg: &Message) -> Decision {
    let Content::Text { value } = &msg.content else {
        return Decision::Accept;
    };
    let t = value.trim();
    if t.is_empty() {
        return Decision::Reject("empty");
    }
    let lower = t.to_lowercase();
    if matches!(msg.role, Role::System) {
        for p in HEARTBEAT {
            if lower == *p || lower.starts_with(p) {
                return Decision::Reject("heartbeat");
            }
        }
    }
    if matches!(msg.role, Role::Assistant) {
        let hits = RESTATEMENT.iter().filter(|m| lower.contains(*m)).count();
        if hits >= 2 {
            return Decision::Reject("system-prompt restatement");
        }
    }
    Decision::Accept
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Content, Message, Role, Source};

    fn msg(role: Role, text: &str) -> Message {
        Message {
            v: 1,
            ts: chrono::Utc::now(),
            src: Source::ClaudeCode,
            conv: "c".into(),
            role,
            content: Content::Text { value: text.into() },
            meta: serde_json::Value::Null,
            refs: vec![],
        }
    }

    #[test]
    fn empty_rejected() {
        assert!(matches!(decide(&msg(Role::User, "")), Decision::Reject(_)));
        assert!(matches!(
            decide(&msg(Role::User, "   \n")),
            Decision::Reject(_)
        ));
    }

    #[test]
    fn heartbeat_rejected() {
        assert!(matches!(
            decide(&msg(Role::System, "[heartbeat]")),
            Decision::Reject(_)
        ));
        assert!(matches!(
            decide(&msg(Role::System, "ping")),
            Decision::Reject(_)
        ));
    }

    #[test]
    fn system_restatement_rejected() {
        let t = "You are Claude, created by Anthropic. You are helpful, harmless, and honest.";
        assert!(matches!(
            decide(&msg(Role::Assistant, t)),
            Decision::Reject(_)
        ));
    }

    #[test]
    fn normal_accepted() {
        assert!(matches!(
            decide(&msg(Role::User, "read src/main.rs please")),
            Decision::Accept
        ));
    }

    #[test]
    fn tool_ref_always_accepted() {
        let m = Message {
            content: Content::ToolRef {
                sha256: "a".into(),
                path: "p".into(),
                bytes: 1,
                desc: "d".into(),
            },
            ..msg(Role::Tool, "")
        };
        assert!(matches!(decide(&m), Decision::Accept));
    }
}
