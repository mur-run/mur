use crate::hitl::tool_tier::{is_spend_tool, spend_floor};
use crate::hitl::{RiskTier, tier_may_be_granted, tier_may_be_waived};

#[test]
fn tier_may_be_waived_table() {
    let table = [
        (Some(RiskTier::Read), true),
        (Some(RiskTier::Write), true),
        (Some(RiskTier::NetworkEgress), true),
        (Some(RiskTier::Destructive), true),
        (Some(RiskTier::Spend), false),
        (Some(RiskTier::Privileged), false),
        (None, false),
    ];
    for (tier, want) in table {
        assert_eq!(tier_may_be_waived(tier), want, "{tier:?}");
    }
}

#[test]
fn tier_may_be_granted_is_unchanged() {
    assert!(!tier_may_be_granted(RiskTier::Destructive));
    assert!(!tier_may_be_granted(RiskTier::NetworkEgress));
}

#[test]
fn spend_tools_match_by_leaf() {
    for name in [
        "fleet_run",
        "parallel_jobs",
        "delegate_to",
        "mcp__mur-tools__parallel_jobs",
    ] {
        assert!(is_spend_tool(name), "{name}");
        assert_eq!(spend_floor(name), Some(RiskTier::Spend), "{name}");
    }
    for name in ["bash", "fleet_status"] {
        assert!(!is_spend_tool(name), "{name}");
        assert_eq!(spend_floor(name), None, "{name}");
    }
}
