//! Gate B's pin for an ssh-agent sign request (ssh-agent proxy spec, "HITL").
//!
//! The unit the user decides about is "this agent may sign with this key for
//! this host key", so exactly those three go into the hash. Session IDs, user
//! names and the signed data are left out: they change on every login, and
//! hashing them would make every remembered approval miss.

/// The `tool` slot. Not a name any chat tool can have (chat tools are plain
/// identifiers), so an ssh pin can never equal a chat pin.
pub const SSH_SIGN_TOOL: &str = "ssh-agent/sign";

/// The `step_or_call_id` slot. Distinct from `CHAT_GATE_STEP` as a second
/// guard against a collision with a chat pin.
pub const SSH_GATE_STEP: &str = "ssh-sign";

/// Canonical hash of an ssh sign request. Same pin as the chat gate, with the
/// channel slot empty (a sign request is not bound to a channel).
pub fn ssh_action_hash(agent: &str, key_fp: &str, host_key_fp: &str) -> String {
    let input = serde_json::json!({ "key_fp": key_fp, "host_key_fp": host_key_fp });
    mur_common::hitl::pin::action_hash(SSH_SIGN_TOOL, &input, "", SSH_GATE_STEP, agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hitl::store::chat_action_hash;

    const AGENT: &str = "coder";
    const KEY: &str = "SHA256:keyAAAA";
    const HOST: &str = "SHA256:hostBBBB";

    #[test]
    fn stable_across_calls() {
        assert_eq!(
            ssh_action_hash(AGENT, KEY, HOST),
            ssh_action_hash(AGENT, KEY, HOST)
        );
    }

    #[test]
    fn is_the_shared_pin_with_ssh_slots() {
        let want = mur_common::hitl::pin::action_hash(
            SSH_SIGN_TOOL,
            &serde_json::json!({ "key_fp": KEY, "host_key_fp": HOST }),
            "",
            SSH_GATE_STEP,
            AGENT,
        );
        assert_eq!(ssh_action_hash(AGENT, KEY, HOST), want);
    }

    #[test]
    fn each_input_changes_the_hash() {
        let base = ssh_action_hash(AGENT, KEY, HOST);
        assert_ne!(base, ssh_action_hash("other", KEY, HOST));
        assert_ne!(base, ssh_action_hash(AGENT, "SHA256:keyCCCC", HOST));
        assert_ne!(base, ssh_action_hash(AGENT, KEY, "SHA256:hostDDDD"));
    }

    #[test]
    fn key_and_host_are_not_interchangeable() {
        assert_ne!(
            ssh_action_hash(AGENT, KEY, HOST),
            ssh_action_hash(AGENT, HOST, KEY)
        );
    }

    #[test]
    fn never_equals_a_chat_pin_with_the_same_fields() {
        let input = serde_json::json!({ "key_fp": KEY, "host_key_fp": HOST });
        let ssh = ssh_action_hash(AGENT, KEY, HOST);
        assert_ne!(ssh, chat_action_hash(SSH_SIGN_TOOL, &input, AGENT));
        assert_ne!(ssh, chat_action_hash("ssh", &input, AGENT));
    }
}
