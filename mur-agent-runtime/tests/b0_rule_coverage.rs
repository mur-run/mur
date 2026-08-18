//! Which test owns which B0 rule.
//!
//! B0 has twelve rules and the tests that cover them do NOT all follow the
//! `b0_ruleN_*.rs` naming. Reading coverage off the filenames therefore
//! under-reports it, and that is not hypothetical: #809 concluded from the
//! file listing that rules 4, 9 and 10 were untested. All three were wrong —
//! rule 4 has three integration tests under different names, rule 9 is covered
//! by unit tests inside `telemetry_writer.rs`, and rule 10 is not a mechanism
//! at all.
//!
//! So the map lives here, as data, next to the tests it indexes, and a test
//! asserts every path in it still exists. A renamed or deleted owner fails
//! loudly instead of quietly turning a rule back into an apparent gap.
//!
//! This does NOT verify that an owner actually exercises its rule — no
//! automated check can — only that the mapping is not stale. It is an index,
//! not a proof.

use std::path::{Path, PathBuf};

/// One B0 rule and the files that cover it.
struct RuleCoverage {
    rule: u8,
    /// What the rule enforces, in one line.
    enforces: &'static str,
    /// Paths relative to the repository root. Empty means "no executable
    /// owner", which is only legitimate with an explanation in `note`.
    owners: &'static [&'static str],
    /// Why the owners are what they are — especially when they are not the
    /// `b0_ruleN_*.rs` file a reader would expect.
    note: &'static str,
}

const COVERAGE: &[RuleCoverage] = &[
    RuleCoverage {
        rule: 1,
        enforces: "fs.write/delete/append/create outside agent_home -> AskUser",
        owners: &["mur-agent-runtime/tests/b0_rule1_fs_confinement.rs"],
        note: "",
    },
    RuleCoverage {
        rule: 2,
        enforces: "network.* against outbound mode Off / Restricted allow_hosts",
        owners: &["mur-agent-runtime/tests/b0_rule2_network_allowlist.rs"],
        note: "",
    },
    RuleCoverage {
        rule: 3,
        enforces: "prior role:tool messages wrapped as <untrusted_tool_result>",
        owners: &["mur-agent-runtime/tests/b0_rule3_spotlight_tool_results.rs"],
        note: "",
    },
    RuleCoverage {
        rule: 4,
        enforces: "no same-turn side-effect tool after fresh untrusted input",
        owners: &[
            "mur-agent-runtime/tests/b0_side_effect_deny.rs",
            "mur-agent-runtime/tests/b0_after_card_import_deny.rs",
            "mur-agent-runtime/tests/b0_share_wrapping.rs",
        ],
        note: "No `b0_rule4_*.rs` file exists and none is needed: the M3.8.2 \
               side-effect test IS rule 4, and the card-import and Track C3 \
               share tests drive the same turn-flag through their own sources. \
               This is the entry #809 read as a gap.",
    },
    RuleCoverage {
        rule: 5,
        enforces: "process.spawn / shell against entitlements.processes.spawn",
        owners: &["mur-agent-runtime/tests/b0_rule5_spawn_deny.rs"],
        note: "",
    },
    RuleCoverage {
        rule: 6,
        enforces: "install-time MCP binary SHA-256 pin refuses startup on drift",
        owners: &["mur-agent-runtime/tests/b0_rule6_mcp_pin.rs"],
        note: "",
    },
    RuleCoverage {
        rule: 7,
        enforces: "outbound message body scanned for credential patterns",
        owners: &["mur-agent-runtime/tests/b0_rule7_secret_prefilter.rs"],
        note: "",
    },
    RuleCoverage {
        rule: 8,
        enforces: "memory.* tool results redacted before persistence",
        owners: &["mur-agent-runtime/tests/b0_rule8_memory_redaction.rs"],
        note: "",
    },
    RuleCoverage {
        rule: 9,
        enforces: "telemetry sink redaction before anything reaches disk",
        owners: &[
            "mur-agent-runtime/src/telemetry_writer.rs",
            "mur-agent-runtime/tests/telemetry.rs",
        ],
        note: "Covered by unit tests INSIDE the writer (`redact_envelope`), not \
               by a file under tests/. A tests/-only survey misses it entirely, \
               which is how #809 recorded it as deferred and untested.",
    },
    RuleCoverage {
        rule: 10,
        enforces: "three-tier permission UX: silent / first-use-remember / always-prompt",
        owners: &[],
        note: "Deliberately no owner. Rule 10 is DOCUMENTATION describing how \
               three already-implemented mechanisms cohabit (M0 silent, M7.3 \
               first-use-remember, M3.8 always-prompt-after-untrusted); each is \
               covered under its own rule. A test for rule 10 would be a \
               category error, not a missing test.",
    },
    RuleCoverage {
        rule: 11,
        enforces: "MCP binary signature verified at startup",
        owners: &["mur-agent-runtime/tests/b0_rule11_mcp_signature.rs"],
        note: "",
    },
    RuleCoverage {
        rule: 12,
        enforces: "companion proactive messaging defaults to quiet",
        owners: &["mur-agent-runtime/tests/companion_gating.rs"],
        note: "Enforced by the M2.x companion subsystem, so its owner sits with \
               the companion tests rather than under a b0_ name.",
    },
];

/// The repository root, from this crate's manifest directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("mur-agent-runtime always has a parent directory")
        .to_path_buf()
}

/// Every mapped owner must still exist. This is what stops the map from
/// rotting into the same wrong conclusion it was written to prevent: rename a
/// test and this fails, instead of the rule quietly looking uncovered again.
#[test]
fn every_mapped_owner_still_exists() {
    let root = repo_root();
    let mut missing: Vec<String> = Vec::new();

    for entry in COVERAGE {
        for owner in entry.owners {
            if !root.join(owner).exists() {
                missing.push(format!("rule {} ({}): {owner}", entry.rule, entry.enforces));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "B0 rule coverage map is stale — these owners no longer exist: {}. \
         Update COVERAGE in this file rather than deleting the entry, so the \
         rule does not silently read as uncovered.",
        missing.join(", ")
    );
}

/// A rule with no executable owner must say why. Otherwise an empty `owners`
/// is indistinguishable from a rule someone forgot to cover — which is exactly
/// the ambiguity this file exists to remove.
#[test]
fn a_rule_without_an_owner_must_explain_itself() {
    for entry in COVERAGE {
        if entry.owners.is_empty() {
            assert!(
                !entry.note.is_empty(),
                "rule {} ({}) has no owner and no explanation — either add the \
                 test that covers it, or record why one cannot exist",
                entry.rule,
                entry.enforces
            );
        }
    }
}

/// The map must cover rules 1..=12 exactly once. A rule added to B0 without an
/// entry here fails the build, so the next person's survey starts from a
/// complete list instead of a directory listing.
#[test]
fn all_twelve_rules_are_accounted_for() {
    let mut seen: Vec<u8> = COVERAGE.iter().map(|c| c.rule).collect();
    seen.sort_unstable();
    let expected: Vec<u8> = (1..=12).collect();
    assert_eq!(
        seen, expected,
        "B0 has twelve rules; the coverage map must list each exactly once"
    );
}
