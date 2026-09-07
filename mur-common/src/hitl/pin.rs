//! Canonical SHA-256 pin of an action. Computed at gate time, embedded in the
//! HitlRequest, and RE-COMPUTED at the execute boundary — fail-closed on drift
//! (defeats the "approve A, execute B" bug). NOT `fingerprint_args` (that uses a
//! build-local DefaultHasher, unfit for a durable cross-process pin).

use sha2::{Digest, Sha256};

/// Canonicalization version — bump if the canonical form changes, so an old
/// pinned hash is never silently compared against a new canonicalization.
pub const PIN_CANON_VERSION: u32 = 1;

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
        "input": input,
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
}
