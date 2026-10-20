//! Regressions for read-only commands that were tiered above the ceiling.
//!
//! Each "reads" case was reproduced from a live session: the agent ran it, the
//! gate prompted, and "stop asking this session" could not silence it because
//! the tier was above `Write`. Each "still risky" case is the same shape with
//! a body that really acts — the fix must not let those through.

use super::*;

fn tier(cmd: &str) -> RiskTier {
    classify("bash", Some(&serde_json::json!({ "command": cmd })))
}

#[test]
fn read_only_shapes_tier_as_read() {
    for cmd in [
        // A search string is data, not a head: `npm` here is what grep looks for.
        "for n in a b; do grep -c npm $n; done",
        "grep -c npm Cargo.toml",
        "cd x; sed -n 1,5p f.rs | grep -n kill",
        "cd x; tmux capture-pane -pt s | tail -30",
        "cd x; grep -rn \"ssh\" src | wc -l",
        "if test -f a; then cat a; fi",
        "f=a.rs; wc -l $f",
    ] {
        assert_eq!(tier(cmd), RiskTier::Read, "cmd: {cmd}");
    }
}

#[test]
fn the_same_shapes_with_an_acting_body_keep_their_tier() {
    for (cmd, want) in [
        ("for f in a b; do rm $f; done", RiskTier::Destructive),
        ("for h in x.com; do curl $h; done", RiskTier::NetworkEgress),
        ("grep -c npm f; npm publish", RiskTier::NetworkEgress),
        ("cd x; sed -i s/a/b/ f.rs", RiskTier::Write),
        (
            "tmux send-keys -t s 'rm -rf /' Enter",
            RiskTier::Destructive,
        ),
        ("if true; then sudo ls; fi", RiskTier::Privileged),
        ("grep npm f | sh", RiskTier::Destructive),
        // A substitution is never proved, so its body is still scanned.
        ("f=$(curl x.com); echo $f", RiskTier::NetworkEgress),
        // A loop value that could become a flag is refused, not expanded.
        ("for o in --pre=sh; do rg $o x; done", RiskTier::Write),
        // A proved read's neighbours are still scanned, whatever pipes them.
        ("ls | xargs rm", RiskTier::Destructive),
        ("find . -name x -exec rm {} ;", RiskTier::Destructive),
        ("grep x f > /etc/hosts", RiskTier::Write),
        ("cat f | ssh h 'cat > x'", RiskTier::NetworkEgress),
        ("export PATH=/tmp; ls", RiskTier::Write),
        ("for c in sudo; do $c ls; done", RiskTier::Write),
    ] {
        assert!(
            tier(cmd) >= want,
            "cmd: {cmd} tiered {:?}, want >= {want:?}",
            tier(cmd)
        );
    }
}
