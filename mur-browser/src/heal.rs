//! Offline self-healing for `mur browser replay --heal`.
//!
//! Design: `docs/superpowers/specs/2026-09-24-browser-replay-heal-design.md`.
//! This module holds the heal budget (D4) and the offline node matcher (D1);
//! verification and write-back land in later steps of plan Task 5.

use std::collections::BTreeSet;

use unicode_normalization::UnicodeNormalization;

use serde::{Deserialize, Serialize};

use crate::locator::{Locator, SnapshotNode, candidates_for_ref};
use crate::recorder::{Action, Step};

/// Default share of element steps allowed to heal in `mode: test`.
pub const DEFAULT_HEAL_RATIO: f32 = 0.2;

/// Tolerance added before `floor` so `f32` ratios such as `0.7` (stored as
/// `0.69999998…`) still give the exact integer the user wrote, e.g. `10 × 0.7 → 7`.
const RATIO_EPSILON: f64 = 1e-6;

/// How many element steps may heal out of `total` at `max_ratio`.
///
/// `total == 0` allows nothing; otherwise at least one heal is always allowed,
/// so short recordings are not failed by a single stale locator.
pub fn allowed_heals(total: u32, max_ratio: f32) -> u32 {
    if total == 0 {
        return 0;
    }
    let raw = (f64::from(total) * f64::from(max_ratio) + RATIO_EPSILON).floor();
    // `raw` is in 0..=total because callers validate max_ratio to 0.0..=1.0;
    // clamp anyway so a bad ratio can never widen the budget past `total`.
    let floored = raw.clamp(0.0, f64::from(total)) as u32;
    floored.max(1)
}

/// Whether `healed` element steps exceed the budget for `total` at `max_ratio`.
pub fn budget_exceeded(healed: u32, total: u32, max_ratio: f32) -> bool {
    healed > allowed_heals(total, max_ratio)
}

/// An "element step" (spec 名詞): it resolves a locator and hands the ref to
/// Playwright. `assert_text` resolves one too but only sends `{text}`, so it
/// is never healed, never verifies a heal, and is not in the budget's
/// denominator.
pub fn is_element_step(action: Action) -> bool {
    action.needs_locator() && action != Action::AssertText
}

/// Lifecycle of one heal (D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealStatus {
    /// Healed; waiting for the next element step to hit directly.
    Pending,
    /// The next element step hit directly. Eligible for write-back.
    Verified,
    /// No later element step could confirm it. Passes, never written back.
    Unverified,
    /// A later step failed (or needed a heal too); the healed step is `Failed`.
    RolledBack,
}

/// One heal, as recorded in the replay report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealEvent {
    pub step: u32,
    /// The step's `locators[]` before the heal.
    pub from: Vec<String>,
    /// Every locator `candidates_for_ref` produced for the chosen node, in
    /// priority order. Write-back merges it via [`prepend_locators`], which
    /// dedupes; the CLI has no snapshot to rebuild this from later.
    pub to: Vec<String>,
    /// `role "name"` of the chosen node, for humans.
    pub node: String,
    pub score: f32,
    /// Why a heal was needed (which candidates missed).
    pub reason: String,
    pub status: HealStatus,
}

/// Minimum similarity for a heal candidate (D2). Provisional; calibrated
/// against the fixtures below in plan Task 5.4 and written back to the spec.
pub const HEAL_MIN_SCORE: f32 = 0.3;

/// Minimum lead of the best candidate over the runner-up (D2). Provisional.
pub const HEAL_MIN_MARGIN: f32 = 0.15;

/// One scored snapshot node, for adoption or for the rejection message.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    /// `role "name"` of the node, for humans.
    pub node: String,
    pub score: f32,
}

/// A node the matcher is confident enough to heal onto.
#[derive(Debug, Clone, PartialEq)]
pub struct HealMatch {
    /// Snapshot ref of the chosen node; used for this replay only, never stored.
    pub reference: String,
    pub chosen: Scored,
    /// Every locator `candidates_for_ref` produced for the node, in priority
    /// order. Never contains an `@ref`. Merge with [`prepend_locators`].
    pub to: Vec<String>,
}

/// Why the matcher declined to heal.
#[derive(Debug, Clone, PartialEq)]
pub enum HealMiss {
    /// The step has no `role:` locator to anchor the search on.
    NoRoleLocator,
    /// No snapshot node shares the step's role.
    NoSameRole { role: String },
    /// Candidates exist but none clears the score or the margin; best first,
    /// at most two.
    NotConfident { top: Vec<Scored> },
}

impl std::fmt::Display for HealMiss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HealMiss::NoRoleLocator => f.write_str("no role locator to anchor on"),
            HealMiss::NoSameRole { role } => write!(f, "no {role:?} node in the snapshot"),
            HealMiss::NotConfident { top } => {
                write!(
                    f,
                    "no confident match (need score >= {HEAL_MIN_SCORE:.2} and lead >= {HEAL_MIN_MARGIN:.2}); top candidates:"
                )?;
                for (i, s) in top.iter().enumerate() {
                    let sep = if i == 0 { " " } else { ", " };
                    write!(f, "{sep}{} {:.2}", s.node, s.score)?;
                }
                Ok(())
            }
        }
    }
}

/// Tolerance for comparing `f32` scores against the thresholds, so a lead of
/// exactly `HEAL_MIN_MARGIN` is not lost to rounding.
const SCORE_EPSILON: f32 = 1e-6;

/// Find the node a stale step most likely meant (D1). Pure: no I/O.
pub fn find_heal(step: &Step, snapshot: &[SnapshotNode]) -> Result<HealMatch, HealMiss> {
    let parsed: Vec<Locator> = step
        .locators
        .iter()
        .filter_map(|l| Locator::parse(l).ok())
        .collect();
    let role = parsed
        .iter()
        .find_map(|l| match l {
            Locator::Role { role, .. } => Some(role.clone()),
            _ => None,
        })
        .ok_or(HealMiss::NoRoleLocator)?;

    let clues = clue_token_sets(step, &parsed);
    let mut scored: Vec<(&SnapshotNode, f32)> = snapshot
        .iter()
        .filter(|node| node.role.eq_ignore_ascii_case(&role))
        .map(|node| (node, node_score(&clues, node)))
        .collect();
    if scored.is_empty() {
        return Err(HealMiss::NoSameRole { role });
    }
    // Stable sort: equal scores keep snapshot order.
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));

    let (best, best_score) = scored[0];
    let runner_up = scored.get(1).map_or(0.0, |(_, s)| *s);
    let confident = best_score + SCORE_EPSILON >= HEAL_MIN_SCORE
        && best_score - runner_up + SCORE_EPSILON >= HEAL_MIN_MARGIN;
    if !confident {
        let top = scored
            .iter()
            .take(2)
            .map(|(node, score)| Scored {
                node: describe(node),
                score: *score,
            })
            .collect();
        return Err(HealMiss::NotConfident { top });
    }
    Ok(HealMatch {
        reference: best.reference.clone(),
        chosen: Scored {
            node: describe(best),
            score: best_score,
        },
        to: candidates_for_ref(&best.reference, snapshot),
    })
}

/// The step's locators after a heal: `to` first, then the old ones, no duplicates.
pub fn prepend_locators(from: &[String], to: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(from.len() + to.len());
    for l in to.iter().chain(from) {
        if !out.contains(l) {
            out.push(l.clone());
        }
    }
    out
}

/// `role "name"` for messages; falls back to visible text, then bare role.
fn describe(node: &SnapshotNode) -> String {
    let shown = if node.name.is_empty() {
        &node.text
    } else {
        &node.name
    };
    if shown.is_empty() {
        node.role.clone()
    } else {
        format!("{} {shown:?}", node.role)
    }
}

/// One token set per clue (D1 step 2): each old role name / text / label, and
/// the intent. Kept separate so a long intent cannot dilute a short name.
fn clue_token_sets(step: &Step, parsed: &[Locator]) -> Vec<BTreeSet<String>> {
    let mut texts: Vec<&str> = parsed
        .iter()
        .filter_map(|l| match l {
            Locator::Role { name, .. } => name.as_deref(),
            Locator::Text(t) | Locator::Label(t) => Some(t.as_str()),
            Locator::TestId(_) | Locator::Css(_) => None,
        })
        .collect();
    texts.push(&step.intent);
    texts
        .into_iter()
        .map(tokens)
        .filter(|set| !set.is_empty())
        .collect()
}

/// Best Jaccard overlap between any clue and any of the node's name/text/label.
fn node_score(clues: &[BTreeSet<String>], node: &SnapshotNode) -> f32 {
    let fields = [Some(&node.name), Some(&node.text), node.label.as_ref()];
    let mut best = 0.0_f32;
    for field in fields.into_iter().flatten() {
        let cand = tokens(field);
        for clue in clues {
            best = best.max(jaccard(clue, &cand));
        }
    }
    best
}

fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f32 {
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(b).count() as f32 / union as f32
}

/// Lowercased, NFKC-normalised tokens: ASCII-style words split on anything
/// non-alphanumeric; CJK runs as character bigrams (a lone character as itself).
fn tokens(s: &str) -> BTreeSet<String> {
    let normal: String = s.nfkc().collect::<String>().to_lowercase();
    let mut out = BTreeSet::new();
    let mut word = String::new();
    let mut cjk: Vec<char> = Vec::new();
    for c in normal.chars() {
        if is_cjk(c) {
            flush_word(&mut word, &mut out);
            cjk.push(c);
        } else if c.is_alphanumeric() {
            flush_cjk(&mut cjk, &mut out);
            word.push(c);
        } else {
            flush_word(&mut word, &mut out);
            flush_cjk(&mut cjk, &mut out);
        }
    }
    flush_word(&mut word, &mut out);
    flush_cjk(&mut cjk, &mut out);
    out
}

fn flush_word(word: &mut String, out: &mut BTreeSet<String>) {
    if !word.is_empty() {
        out.insert(std::mem::take(word));
    }
}

fn flush_cjk(run: &mut Vec<char>, out: &mut BTreeSet<String>) {
    match run.len() {
        0 => {}
        1 => {
            out.insert(run[0].to_string());
        }
        _ => {
            for pair in run.windows(2) {
                out.insert(pair.iter().collect());
            }
        }
    }
    run.clear();
}

/// Scripts written without spaces between words.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF   // Hiragana, Katakana
        | 0x3400..=0x4DBF // CJK Extension A
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0x20000..=0x2FFFF // CJK Extensions B+
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_within_ratio_is_not_exceeded() {
        assert!(!budget_exceeded(2, 10, 0.2));
    }

    #[test]
    fn budget_over_ratio_is_exceeded() {
        assert!(budget_exceeded(3, 10, 0.2));
    }

    #[test]
    fn budget_short_run_always_allows_one_heal() {
        assert!(!budget_exceeded(1, 3, 0.2));
    }

    #[test]
    fn budget_short_run_second_heal_is_exceeded() {
        assert!(budget_exceeded(2, 3, 0.2));
    }

    #[test]
    fn budget_no_element_steps_is_never_exceeded() {
        assert!(!budget_exceeded(0, 0, 0.2));
        assert_eq!(allowed_heals(0, 0.2), 0);
    }

    #[test]
    fn allowed_heals_is_exact_for_f32_ratios() {
        assert_eq!(allowed_heals(10, 0.2), 2);
        assert_eq!(allowed_heals(10, 0.7), 7);
        assert_eq!(allowed_heals(10, DEFAULT_HEAL_RATIO), 2);
    }

    #[test]
    fn allowed_heals_never_exceeds_total() {
        assert_eq!(allowed_heals(3, 1.0), 3);
        assert_eq!(allowed_heals(3, 5.0), 3);
        assert_eq!(allowed_heals(3, 0.0), 1);
    }

    // ---- matcher (D1/D2) --------------------------------------------------
    //
    // These call `find_heal` directly. Whether a step may reach the matcher
    // at all (LocateMiss, no testid candidate) is `run_step`'s job, not
    // tested here.

    use crate::locator::parse_snapshot;
    use crate::recorder::Action;

    fn step(intent: &str, locators: &[&str]) -> Step {
        Step {
            step: 3,
            intent: intent.to_string(),
            intent_auto: false,
            action: Action::Click,
            value: None,
            locators: locators.iter().map(|l| l.to_string()).collect(),
            healed: false,
            last_hit: 0,
            ref_at_record: Some("e9".to_string()),
        }
    }

    #[test]
    fn heal_adopts_renamed_testid_button() {
        let step = step(
            "Place the order",
            &["role:button[name=\"Place order\"]", "testid:place-order"],
        );
        let snap = parse_snapshot(
            r#"
- main:
  - button "Cancel" [ref=e4]
  - button "Place your order" [ref=e5] [data-testid=checkout-submit]
"#,
        );
        let hit = find_heal(&step, &snap).expect("should heal onto e5");
        assert_eq!(hit.reference, "e5");
        assert!(hit.chosen.node.contains("Place your order"));
        assert!(hit.to.contains(&"testid:checkout-submit".to_string()));
    }

    #[test]
    fn heal_adopts_cjk_text_tweak() {
        let step = step("點擊送出按鈕", &["role:button[name=\"送出\"]"]);
        let snap = parse_snapshot(
            r#"
- main:
  - button "取消" [ref=e1]
  - button "送出訂單" [ref=e2]
"#,
        );
        let hit = find_heal(&step, &snap).expect("「送出」→「送出訂單」 should heal");
        assert_eq!(hit.reference, "e2");
        assert_eq!(hit.to[0], "role:button[name=\"送出訂單\"]");
    }

    #[test]
    fn heal_rejects_close_same_role_candidates() {
        let step = step("Save it", &["role:button[name=\"Save\"]"]);
        let snap = parse_snapshot(
            r#"
- main:
  - button "Save draft" [ref=e1]
  - button "Save changes" [ref=e2]
"#,
        );
        match find_heal(&step, &snap) {
            Err(HealMiss::NotConfident { top }) => {
                assert_eq!(top.len(), 2);
                let names: Vec<_> = top.iter().map(|s| s.node.as_str()).collect();
                assert!(names.iter().any(|n| n.contains("Save draft")));
                assert!(names.iter().any(|n| n.contains("Save changes")));
            }
            other => panic!("expected NotConfident, got {other:?}"),
        }
    }

    #[test]
    fn heal_rejection_message_lists_top_two_with_scores() {
        let miss = HealMiss::NotConfident {
            top: vec![
                Scored {
                    node: "button \"Save draft\"".into(),
                    score: 0.5,
                },
                Scored {
                    node: "button \"Save changes\"".into(),
                    score: 0.5,
                },
            ],
        };
        let msg = miss.to_string();
        assert!(
            msg.contains("Save draft") && msg.contains("Save changes"),
            "{msg}"
        );
        assert!(msg.contains("0.50"), "{msg}");
    }

    #[test]
    fn heal_rejects_step_without_role_locator() {
        let step = step("Submit the form", &["text:Submit", "label:Submit form"]);
        let snap = parse_snapshot(r#"- button "Submit now" [ref=e1]"#);
        assert_eq!(find_heal(&step, &snap), Err(HealMiss::NoRoleLocator));
        assert_eq!(
            HealMiss::NoRoleLocator.to_string(),
            "no role locator to anchor on"
        );
    }

    #[test]
    fn heal_rejects_different_role() {
        let step = step("Go to checkout", &["role:button[name=\"Checkout\"]"]);
        let snap = parse_snapshot(r#"- link "Checkout" [ref=e1]"#);
        assert_eq!(
            find_heal(&step, &snap),
            Err(HealMiss::NoSameRole {
                role: "button".into()
            })
        );
    }

    #[test]
    fn heal_role_match_ignores_case() {
        let step = step("Sign in", &["role:Button[name=\"Sign in\"]"]);
        let snap = parse_snapshot(r#"- button "Sign in now" [ref=e1]"#);
        assert_eq!(
            find_heal(&step, &snap).map(|h| h.reference),
            Ok("e1".into())
        );
    }

    #[test]
    fn heal_new_locators_never_carry_a_ref() {
        let step = step("Search the docs", &["role:searchbox[name=\"Search\"]"]);
        let snap = parse_snapshot(r#"- searchbox "Search docs" [ref=e3] [label="Search docs"]"#);
        let hit = find_heal(&step, &snap).expect("should heal");
        assert!(!hit.to.is_empty());
        assert!(
            hit.to.iter().all(|l| !l.contains('@') && !l.contains("e3")),
            "{:?}",
            hit.to
        );
    }

    #[test]
    fn heal_rejects_lone_unrelated_node_below_score_floor() {
        let step = step(
            "Download the invoice",
            &["role:button[name=\"Download invoice\"]"],
        );
        let snap = parse_snapshot(r#"- button "Help" [ref=e1]"#);
        match find_heal(&step, &snap) {
            Err(HealMiss::NotConfident { top }) => assert_eq!(top.len(), 1),
            other => panic!("expected NotConfident, got {other:?}"),
        }
    }

    /// Calibration record (spec D1 constants table). If a tokenizer or
    /// scoring change moves these, re-check `HEAL_MIN_SCORE` / `HEAL_MIN_MARGIN`.
    #[test]
    fn heal_calibration_scores_are_pinned() {
        let cases = [
            (
                step("Place the order", &["role:button[name=\"Place order\"]"]),
                "Place your order",
                2.0 / 3.0,
            ),
            (
                step("點擊送出按鈕", &["role:button[name=\"送出\"]"]),
                "送出訂單",
                1.0 / 3.0,
            ),
            (
                step("Save it", &["role:button[name=\"Save\"]"]),
                "Save draft",
                0.5,
            ),
        ];
        for (step, name, want) in cases {
            let snap = parse_snapshot(&format!("- button {name:?} [ref=e1]"));
            let got = find_heal(&step, &snap).expect("lone node clears the floor");
            assert!(
                (got.chosen.score - want).abs() < 1e-4,
                "{name}: {}",
                got.chosen.score
            );
            assert!(
                got.chosen.score >= HEAL_MIN_SCORE,
                "{name} fell below the floor"
            );
        }
    }

    #[test]
    fn prepend_puts_new_first_and_drops_duplicates() {
        let from = vec![
            "role:button[name=\"Save\"]".to_string(),
            "text:Save".to_string(),
        ];
        let to = vec![
            "role:button[name=\"Save all\"]".to_string(),
            "text:Save".to_string(),
        ];
        assert_eq!(
            prepend_locators(&from, &to),
            vec![
                "role:button[name=\"Save all\"]".to_string(),
                "text:Save".to_string(),
                "role:button[name=\"Save\"]".to_string(),
            ]
        );
    }
}
