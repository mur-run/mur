use super::*;
use crate::agent::{ToolPolicy, ToolRule};

fn rule(pattern: &str, waiver: FloorWaiver) -> ToolRule {
    ToolRule {
        pattern: pattern.into(),
        policy: ToolPolicy::Allow,
        risk: None,
        bypass_builtin_tier_floor: waiver,
    }
}

fn parse(yaml: &str) -> Result<FloorWaiver, serde_yaml::Error> {
    serde_yaml::from_str(yaml)
}

#[test]
fn parses_bools_and_tier_names() {
    assert_eq!(parse("false").unwrap(), FloorWaiver::Off);
    assert_eq!(parse("true").unwrap(), FloorWaiver::All);
    assert_eq!(
        parse("network-egress").unwrap(),
        FloorWaiver::UpTo(RiskTier::NetworkEgress)
    );
    assert_eq!(
        parse("destructive").unwrap(),
        FloorWaiver::UpTo(RiskTier::Destructive)
    );
    let err = parse("yes-please").unwrap_err().to_string();
    assert!(err.contains("bypass_builtin_tier_floor"), "{err}");
}

/// Old profiles (`true` / absent) round-trip unchanged; absent stays absent.
#[test]
fn serializes_back_to_what_was_written() {
    for (v, s) in [
        (FloorWaiver::All, "true"),
        (FloorWaiver::Off, "false"),
        (FloorWaiver::UpTo(RiskTier::NetworkEgress), "network-egress"),
    ] {
        assert_eq!(serde_yaml::to_string(&v).unwrap().trim(), s);
    }
    let r: ToolRule = serde_yaml::from_str("pattern: bash\npolicy: allow\n").unwrap();
    assert!(r.bypass_builtin_tier_floor.is_off());
    assert!(
        !serde_yaml::to_string(&r)
            .unwrap()
            .contains("bypass_builtin_tier_floor")
    );
}

#[test]
fn bash_ceiling_defaults_to_write_and_ignores_true() {
    assert_eq!(bash_ceiling(None), RiskTier::Write);
    assert_eq!(
        bash_ceiling(Some(&rule("bash", FloorWaiver::Off))),
        RiskTier::Write
    );
    assert_eq!(
        bash_ceiling(Some(&rule("bash", FloorWaiver::All))),
        RiskTier::Write,
        "`true` on bash would waive every command"
    );
}

#[test]
fn bash_ceiling_honors_a_named_tier_but_never_privileged() {
    assert_eq!(
        bash_ceiling(Some(&rule(
            "bash",
            FloorWaiver::UpTo(RiskTier::NetworkEgress)
        ))),
        RiskTier::NetworkEgress
    );
    assert_eq!(
        bash_ceiling(Some(&rule("bash", FloorWaiver::UpTo(RiskTier::Privileged)))),
        RiskTier::Destructive
    );
    // A waiver can only lift, never drop the ceiling under Write.
    assert_eq!(
        bash_ceiling(Some(&rule("bash", FloorWaiver::UpTo(RiskTier::Read)))),
        RiskTier::Write
    );
}

/// #1829: a glob never carries a waiver, even when it decides `bash`.
#[test]
fn bash_ceiling_ignores_a_glob_waiver() {
    assert_eq!(
        bash_ceiling(Some(&rule("*", FloorWaiver::UpTo(RiskTier::Destructive)))),
        RiskTier::Write
    );
    assert_eq!(
        bash_ceiling(Some(&rule("ba*", FloorWaiver::UpTo(RiskTier::Destructive)))),
        RiskTier::Write
    );
}

#[test]
fn covers_compares_against_the_named_tier() {
    let egress = FloorWaiver::UpTo(RiskTier::NetworkEgress);
    assert!(egress.covers(RiskTier::NetworkEgress));
    assert!(!egress.covers(RiskTier::Privileged));
    assert!(FloorWaiver::All.covers(RiskTier::Privileged));
    assert!(!FloorWaiver::Off.covers(RiskTier::Write));
}

#[test]
fn warns_on_true_and_privileged_on_bash_only() {
    let w = waiver_warnings(&[
        rule("bash", FloorWaiver::All),
        rule("mcp__browser__browser_click", FloorWaiver::All),
    ]);
    assert_eq!(w.len(), 1);
    assert!(w[0].contains("IGNORED"), "{}", w[0]);
    let w = waiver_warnings(&[rule("bash", FloorWaiver::UpTo(RiskTier::Privileged))]);
    assert!(w[0].contains("capped at destructive"), "{}", w[0]);
    assert!(
        waiver_warnings(&[rule("bash", FloorWaiver::UpTo(RiskTier::NetworkEgress))]).is_empty()
    );
}

fn cmd(c: &str) -> serde_json::Value {
    serde_json::json!({ "command": c })
}

#[test]
fn ceiling_hit_only_for_bash_above_the_ceiling() {
    let plain = [rule("bash", FloorWaiver::Off)];
    let push = cmd("git push origin main");
    let hit = bash_ceiling_hit(&plain, "bash", Some(&push)).expect("push is egress");
    assert_eq!(hit.tier, RiskTier::NetworkEgress);
    assert_eq!(hit.ceiling, RiskTier::Write);
    assert_eq!(hit.rule.as_deref(), Some("bash"));

    assert_eq!(
        bash_ceiling_hit(&plain, "bash", Some(&cmd("cargo build"))),
        None
    );
    assert_eq!(bash_ceiling_hit(&plain, "edit_file", Some(&push)), None);

    let egress = [rule("bash", FloorWaiver::UpTo(RiskTier::NetworkEgress))];
    assert_eq!(bash_ceiling_hit(&egress, "bash", Some(&push)), None);
    let rm = bash_ceiling_hit(&egress, "bash", Some(&cmd("rm -rf target"))).unwrap();
    assert_eq!(rm.tier, RiskTier::Destructive);

    let max = [rule("bash", FloorWaiver::UpTo(RiskTier::Destructive))];
    let sudo = bash_ceiling_hit(&max, "bash", Some(&cmd("sudo launchctl list"))).unwrap();
    assert_eq!(
        sudo.tier,
        RiskTier::Privileged,
        "privileged always lands above"
    );
}

/// `perm list-tools` / `doctor` describe the bash ceiling from the same rule
/// the runtime reads.
#[test]
fn bash_rules_get_a_floor_note() {
    use crate::hitl::builtin_tier::{FloorNote, floor_note};
    assert_eq!(
        floor_note(&rule("bash", FloorWaiver::Off)),
        Some(FloorNote::BashCeiling {
            ceiling: RiskTier::Write
        })
    );
    assert_eq!(
        floor_note(&rule("bash", FloorWaiver::UpTo(RiskTier::NetworkEgress))),
        Some(FloorNote::BashCeiling {
            ceiling: RiskTier::NetworkEgress
        })
    );
    assert_eq!(
        floor_note(&rule("bash", FloorWaiver::All)),
        Some(FloorNote::BashTrueIgnored)
    );
    let mut ask = rule("bash", FloorWaiver::Off);
    ask.policy = ToolPolicy::Ask;
    assert_eq!(floor_note(&ask), None, "an ask rule already asks");
}
