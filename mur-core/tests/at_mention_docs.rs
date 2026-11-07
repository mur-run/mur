//! at-mention D1: the user-facing docs name every refusal a caller can see.
//! The expected strings come from the error types themselves (their Display
//! and JSON-RPC code), so a new or renamed refusal fails here until the
//! README and the at-mention docs page document it.

use mur_agent_runtime::protocol::a2a_server::HandlerError;
use mur_channel::conversation::ConversationDamage;
use mur_core::cmd::agent::cli::mention_target::TargetError;
use std::path::PathBuf;

/// Docs that document `@` mentions, relative to the workspace root.
const DOCS: &[&str] = &["README.md", "docs/cookbook/at-mention-conversations.md"];

fn read_doc(rel: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Every runtime channel-conversation refusal (at-mention §7).
fn runtime_refusals() -> Vec<HandlerError> {
    let s = || "x".to_string();
    vec![
        HandlerError::UnknownConversation(s()),
        HandlerError::ChannelTooNew(s()),
        HandlerError::ChannelNotConversation(s()),
        HandlerError::NotAParticipant(s()),
        HandlerError::ConversationRepairing(s()),
        HandlerError::ConversationDamaged(ConversationDamage::Chain),
        HandlerError::ConversationBusy(s()),
    ]
}

/// The message prefix a client matches on: Display up to and including `:`.
fn prefix(e: &HandlerError) -> String {
    let msg = e.to_string();
    let end = msg.find(':').expect("refusal display has a ':'");
    msg[..=end].to_string()
}

#[test]
fn docs_list_every_runtime_refusal_with_its_jsonrpc_code() {
    for doc in DOCS {
        let text = read_doc(doc);
        for e in runtime_refusals() {
            let (code, p) = (e.code().to_string(), prefix(&e));
            assert!(
                text.lines().any(|l| l.contains(&code) && l.contains(&p)),
                "{doc}: no line names `{p}` together with code {code}"
            );
        }
    }
}

#[test]
fn docs_list_every_damage_reason() {
    let reasons = [
        ConversationDamage::Manifest {
            detail: String::new(),
        },
        ConversationDamage::Unparseable { lines: vec![] },
        ConversationDamage::Signature,
        ConversationDamage::Chain,
        ConversationDamage::Anchor,
        ConversationDamage::LogRegressed,
        ConversationDamage::Watermark {
            detail: String::new(),
        },
    ];
    for doc in DOCS {
        let text = read_doc(doc);
        for r in &reasons {
            let tick = format!("`{}`", r.reason());
            assert!(
                text.contains(&tick),
                "{doc}: damage reason {tick} undocumented"
            );
        }
    }
}

#[test]
fn docs_list_every_send_refusal_code() {
    let n = || "n".to_string();
    let errs = [
        TargetError::ReservedName(n()),
        TargetError::UnknownTarget(n()),
        TargetError::AmbiguousTarget(n()),
        TargetError::UnknownConversation(1),
        TargetError::ChannelNotConversation(1),
        TargetError::NotAParticipant {
            agent: n(),
            ordinal: 1,
        },
        TargetError::TargetOffline(n()),
    ];
    for doc in DOCS {
        let text = read_doc(doc);
        for e in &errs {
            let msg = e.to_string();
            let code = msg.split(':').next().unwrap();
            assert!(
                text.contains(&format!("`{code}`")),
                "{doc}: `{code}` undocumented"
            );
        }
    }
}

#[test]
fn docs_show_the_mention_forms() {
    for doc in DOCS {
        let text = read_doc(doc);
        let forms = [
            "mur agent send '@",
            "@agent:",
            "@fleet:",
            "/channels",
            "conversation_max_waiting",
        ];
        for form in forms {
            assert!(text.contains(form), "{doc}: missing `{form}`");
        }
    }
}
