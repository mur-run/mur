// mur-research-gateway/src/audit.rs
//
// URL-level audit: every `search`/`fetch` call emits ONE structured
// single-line JSON record via `tracing::info!(target: "research_gateway_audit", ...)`.
// This is the sole request-level evidence for the browser tiers (the egress
// proxy is blind to browser-subprocess connections — spec §7.2/§7.4), so the
// audit must fire on EVERY call: success, denied, AND error — not just the
// happy path.

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct AuditRecord {
    pub worker: Option<String>,
    pub verb: &'static str,
    pub target: String,
    pub tier: Option<u8>,
    pub outcome: &'static str,
}

impl AuditRecord {
    /// Build a record, reading `worker` from the `MUR_AGENT_NAME` env var the
    /// runtime sets on the child (fallback `None` when absent, e.g. local dev).
    pub fn new(
        verb: &'static str,
        target: String,
        tier: Option<u8>,
        outcome: &'static str,
    ) -> Self {
        AuditRecord {
            worker: std::env::var("MUR_AGENT_NAME").ok(),
            verb,
            target,
            tier,
            outcome,
        }
    }
}

/// Render a record as a single-line JSON object. Pure function — kept
/// separate from `audit()` so it's unit-testable without capturing logs.
fn render_audit(record: &AuditRecord) -> String {
    serde_json::to_string(record).unwrap_or_else(|_| "{}".to_string())
}

/// Log the rendered audit line at `tracing::info!(target: "research_gateway_audit", ...)`.
pub fn audit(record: AuditRecord) {
    tracing::info!(target: "research_gateway_audit", "{}", render_audit(&record));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_line_is_single_json_object() {
        let line = super::render_audit(&AuditRecord {
            worker: Some("worker_3".into()),
            verb: "fetch",
            target: "https://example.com".into(),
            tier: Some(1),
            outcome: "ok",
        });
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["verb"], "fetch");
        assert_eq!(v["tier"], 1);
        assert_eq!(v["outcome"], "ok");
    }

    #[test]
    fn audit_line_omits_nothing_when_worker_and_tier_absent() {
        // denied/error paths: tier is None, worker may be None outside a
        // runtime-managed child — both must still serialize to valid JSON.
        let line = render_audit(&AuditRecord {
            worker: None,
            verb: "search",
            target: "swift testing".into(),
            tier: None,
            outcome: "denied",
        });
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["worker"], serde_json::Value::Null);
        assert_eq!(v["tier"], serde_json::Value::Null);
        assert_eq!(v["outcome"], "denied");
    }
}
