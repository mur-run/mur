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

mod waiver {
    use crate::agent::{OverrideFlags, OverrideRecord};
    use crate::hitl::RiskTier;
    use crate::hitl::startup_waiver::{AskOrigin, STARTUP_WAIVER_REASON_PREFIX, StartupWaiver};
    use crate::startup_override::ConfirmedBy;

    fn record(no_hitl: bool) -> OverrideRecord {
        OverrideRecord {
            nonce: "n-1".into(),
            flags: OverrideFlags {
                no_hitl,
                ..OverrideFlags::default()
            },
            confirmed_by: ConfirmedBy::Tty,
        }
    }

    fn waiver() -> StartupWaiver {
        StartupWaiver::from_record(&record(true)).expect("no_hitl builds a waiver")
    }

    fn o(explicit: bool, default: bool, clamp: bool, intent_raised: bool) -> AskOrigin {
        AskOrigin {
            explicit,
            default,
            clamp,
            intent_raised,
            hook_raised: false,
        }
    }

    fn hooked(origin: AskOrigin) -> AskOrigin {
        AskOrigin {
            hook_raised: true,
            ..origin
        }
    }

    #[test]
    fn may_waive_order() {
        let w = waiver();
        let tier = Some(RiskTier::Write);
        let cases = [
            ("default", o(false, true, false, false), true),
            ("clamp", o(false, false, true, false), true),
            ("raise alone", o(false, false, false, true), false),
            ("default + raise", o(false, true, false, true), false),
            ("clamp + raise", o(false, false, true, true), false),
            ("explicit", o(true, false, false, false), false),
            ("explicit + default", o(true, true, false, false), false),
            ("explicit + clamp", o(true, false, true, false), false),
        ];
        for (name, origin, want) in cases {
            assert_eq!(w.may_waive(origin, tier), want, "{name}");
        }
        assert!(!w.may_waive(o(false, true, false, false), Some(RiskTier::Spend)));
    }

    /// B0 rule 4 asks about the turn; a default or clamp underneath it must
    /// not let the waiver answer for the human.
    #[test]
    fn a_hook_ask_is_never_waived() {
        let w = waiver();
        for origin in [o(false, true, false, false), o(false, false, true, false)] {
            assert!(
                !w.may_waive(hooked(origin), Some(RiskTier::Write)),
                "{origin:?}"
            );
        }
    }

    #[test]
    fn default_origin_is_never_waived() {
        assert!(!waiver().may_waive(AskOrigin::default(), Some(RiskTier::Write)));
    }

    #[test]
    fn never_passes_privileged_or_spend() {
        let w = waiver();
        for bits in 0u8..32 {
            let origin = AskOrigin {
                hook_raised: bits & 16 != 0,
                ..o(bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, bits & 8 != 0)
            };
            for tier in [Some(RiskTier::Privileged), Some(RiskTier::Spend), None] {
                assert!(!w.may_waive(origin, tier), "{origin:?} at {tier:?}");
            }
        }
    }

    #[test]
    fn from_record_needs_no_hitl() {
        assert!(StartupWaiver::from_record(&record(false)).is_none());
        let w = waiver();
        assert_eq!(w.nonce(), "n-1");
        assert_eq!(w.reason(), format!("{STARTUP_WAIVER_REASON_PREFIX}n-1"));
        assert_eq!(w.reason(), "startup-override:n-1");
    }
}
