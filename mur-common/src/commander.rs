use serde::{Deserialize, Serialize};

/// Metadata key injected into A2A envelopes that carry a governance directive.
pub const COMMANDER_DIRECTIVE_KEY: &str = "commander_directive";

/// A cryptographically-signed instruction from the Commander governance plane.
///
/// Serialised as a JSON object and placed under [`COMMANDER_DIRECTIVE_KEY`]
/// in an A2A envelope's `metadata` map.  The signing layer lives in
/// `mur-core`; this crate only owns the payload shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommanderDirective {
    /// Directive kind, e.g. `"kill"`, `"budget_ceiling"`.
    pub kind: String,

    /// Target fleet name.
    pub fleet: String,

    /// Optional budget ceiling in USD; `>=0` is honoured, `<0`/`NaN` ignored.
    pub budget_usd: Option<f64>,

    /// Replay-prevention nonce (UUID v4 recommended).
    pub nonce: String,

    /// Wall-clock milliseconds since Unix epoch when the directive was issued.
    pub issued_at_ms: u64,
}

/// Derived governance state for a running fleet loop.
///
/// Built by reducing the stream of [`CommanderDirective`]s received so far.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GovernanceState {
    /// If `true`, the loop must stop immediately.
    pub killed: bool,

    /// Active budget ceiling (USD), if any has been applied.
    pub budget_ceiling: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_roundtrips_under_the_marker_key() {
        let d = CommanderDirective {
            kind: "kill".into(),
            fleet: "dev".into(),
            budget_usd: None,
            nonce: "n1".into(),
            issued_at_ms: 1_750_000_000_000,
        };
        let wrapped = serde_json::json!({ COMMANDER_DIRECTIVE_KEY: &d });
        let got: CommanderDirective =
            serde_json::from_value(wrapped[COMMANDER_DIRECTIVE_KEY].clone()).unwrap();
        assert_eq!(got.kind, "kill");
        assert_eq!(got.fleet, "dev");
        assert_eq!(got.issued_at_ms, 1_750_000_000_000);
    }

    #[test]
    fn governance_state_default_is_inert() {
        let g = GovernanceState::default();
        assert!(!g.killed && g.budget_ceiling.is_none());
    }
}
