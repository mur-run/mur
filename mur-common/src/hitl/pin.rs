//! Canonical SHA-256 pin of an action. Computed at gate time, embedded in the
//! HitlRequest, and RE-COMPUTED at the execute boundary — fail-closed on drift
//! (defeats the "approve A, execute B" bug). NOT `fingerprint_args` (that uses a
//! build-local DefaultHasher, unfit for a durable cross-process pin).

use sha2::{Digest, Sha256};

/// Canonicalization version — bump if the canonical form changes, so an old
/// pinned hash is never silently compared against a new canonicalization.
///
/// v2 drops `NARRATION_FIELDS` from `input`. v1 hashes cannot be compared
/// against v2 hashes, which is the point: settled decisions pinned under v1
/// simply stop matching and are re-asked once.
pub const PIN_CANON_VERSION: u32 = 2;

/// Top-level input keys a tool takes for NARRATION, not for the work.
///
/// Excluded from the pin because they are model prose, not the action. The
/// bash schema asks for a `description` ("what you are doing and why") that
/// the tool never executes; hashing it meant re-running the SAME command with
/// a freshly-worded description minted a different pin, so a remembered
/// approval never matched and the user was asked again — read-tier `gh pr
/// checks` included. Same list the doom-loop fingerprint strips, and for the
/// same reason.
///
/// Safety: narration cannot change what executes, so dropping it cannot let an
/// "approve A, execute B" drift slip past the execute-boundary re-verify —
/// every field that determines behaviour is still hashed.
pub const NARRATION_FIELDS: [&str; 1] = ["description"];

/// `input` with top-level narration keys removed. Nested objects are left
/// alone (a nested `description` may well be payload), and a non-object input
/// is returned untouched.
fn strip_narration(input: &serde_json::Value) -> std::borrow::Cow<'_, serde_json::Value> {
    let Some(map) = input.as_object() else {
        return std::borrow::Cow::Borrowed(input);
    };
    if !NARRATION_FIELDS.iter().any(|k| map.contains_key(*k)) {
        return std::borrow::Cow::Borrowed(input);
    }
    let stripped: serde_json::Map<String, serde_json::Value> = map
        .iter()
        .filter(|(k, _)| !NARRATION_FIELDS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    std::borrow::Cow::Owned(serde_json::Value::Object(stripped))
}

/// SHA-256 hex over the canonical action. `input` must already be the
/// POST-substitution args (what will actually execute). `serde_json` sorts
/// object keys (no preserve_order feature), so the encoding is deterministic.
pub fn action_hash(
    tool_name: &str,
    input: &serde_json::Value,
    channel_id: &str,
    step_or_call_id: &str,
    agent_id: &str,
) -> String {
    let canon = serde_json::json!({
        "v": PIN_CANON_VERSION,
        "tool": tool_name,
        "input": strip_narration(input).as_ref(),
        "channel": channel_id,
        "step": step_or_call_id,
        "agent": agent_id,
    });
    let bytes = serde_json::to_vec(&canon).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(&bytes);
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_order_independent() {
        let a = action_hash("bash", &serde_json::json!({"b":1,"a":2}), "c", "s0", "mur");
        // Same logical input, keys written in a different order → same hash
        // (serde_json sorts object keys).
        let b = action_hash("bash", &serde_json::json!({"a":2,"b":1}), "c", "s0", "mur");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn drift_changes_the_hash() {
        let approved = action_hash("bash", &serde_json::json!({"cmd":"rm a"}), "c", "s0", "mur");
        let executed = action_hash("bash", &serde_json::json!({"cmd":"rm b"}), "c", "s0", "mur");
        assert_ne!(approved, executed, "different args MUST fail the re-verify");
    }

    #[test]
    fn narration_does_not_change_the_hash() {
        let a = action_hash(
            "bash",
            &serde_json::json!({"command":"gh pr checks","description":"Checking CI"}),
            "c",
            "s0",
            "mur",
        );
        let b = action_hash(
            "bash",
            &serde_json::json!({"command":"gh pr checks","description":"Re-checking PR status"}),
            "c",
            "s0",
            "mur",
        );
        let none = action_hash(
            "bash",
            &serde_json::json!({"command":"gh pr checks"}),
            "c",
            "s0",
            "mur",
        );
        assert_eq!(a, b, "re-worded narration must reuse the remembered pin");
        assert_eq!(a, none, "absent narration must pin the same as any wording");
    }

    #[test]
    fn nested_description_is_still_hashed() {
        let a = action_hash(
            "t",
            &serde_json::json!({"x":{"description":"a"}}),
            "c",
            "s",
            "m",
        );
        let b = action_hash(
            "t",
            &serde_json::json!({"x":{"description":"b"}}),
            "c",
            "s",
            "m",
        );
        assert_ne!(a, b, "only TOP-level narration is dropped");
    }

    #[test]
    fn non_object_input_hashes_whole() {
        let a = action_hash("t", &serde_json::json!("raw"), "c", "s", "m");
        let b = action_hash("t", &serde_json::json!("other"), "c", "s", "m");
        assert_ne!(a, b);
    }
}
