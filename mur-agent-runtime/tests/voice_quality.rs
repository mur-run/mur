//! C1 voice-quality linter checks against 9 hand-authored samples.
//!
//! C2 (Spec §8.5): the rendered sample bodies live in
//! `tests/fixtures/voice_quality/*.md` for human review on each PR.
//! When a sample changes, the human reviewer reads them. The C1 test asserts
//! the rendered samples conform to the mechanical linter rules.

use mur_agent_runtime::companion::linter::{Violation, check};
use mur_common::companion::Relationship;
use std::path::PathBuf;

struct Sample {
    file_stem: &'static str,
    relationship: Relationship,
    locale: &'static str,
    situation_slug: &'static str,
    body: &'static str,
}

fn samples() -> Vec<Sample> {
    vec![
        Sample {
            file_stem: "01_friend_en_morning_greeting",
            relationship: Relationship::Friend,
            locale: "en-US",
            situation_slug: "morning_greeting",
            body: "Good morning. Hope today treats you gently.",
        },
        Sample {
            file_stem: "02_friend_zh_gentle_check_in",
            relationship: Relationship::Friend,
            locale: "zh-TW",
            situation_slug: "gentle_check_in",
            body: "嗨,今天還順利嗎?有什麼想聊的隨時開個訊息給我。",
        },
        Sample {
            file_stem: "03_coach_en_share_quote",
            relationship: Relationship::Coach,
            locale: "en-US",
            situation_slug: "share_quote",
            body: "Worth chewing on today: \"The cost of a thing is the amount of life you exchange for it.\"",
        },
        Sample {
            file_stem: "04_coach_zh_share_link",
            relationship: Relationship::Coach,
            locale: "zh-TW",
            situation_slug: "share_link",
            body: "看到一篇覺得對你或許有幫助的文章,放在這裡:https://example.com/article",
        },
        Sample {
            file_stem: "05_buddy_en_gentle_check_in",
            relationship: Relationship::AccountabilityBuddy,
            locale: "en-US",
            situation_slug: "gentle_check_in",
            body: "Quick check: how is the week going? No rush, just curious.",
        },
        Sample {
            file_stem: "06_buddy_zh_morning_greeting",
            relationship: Relationship::AccountabilityBuddy,
            locale: "zh-TW",
            situation_slug: "morning_greeting",
            body: "早安,新的一天。有什麼想完成的小目標嗎?",
        },
        Sample {
            file_stem: "07_mentor_en_share_quote",
            relationship: Relationship::Mentor,
            locale: "en-US",
            situation_slug: "share_quote",
            body: "I keep returning to this idea: progress lives in the small, repeated choices.",
        },
        Sample {
            file_stem: "08_mentor_zh_share_link",
            relationship: Relationship::Mentor,
            locale: "zh-TW",
            situation_slug: "share_link",
            body: "讀到這段覺得想分享給你:https://example.com/note",
        },
        Sample {
            file_stem: "09_friend_en_share_quote",
            relationship: Relationship::Friend,
            locale: "en-US",
            situation_slug: "share_quote",
            body: "Saw this and thought of you: \"What you avoid, you teach.\"",
        },
    ]
}

#[test]
fn all_samples_pass_c1_linter() {
    for s in samples() {
        let report = check(s.body, s.locale);
        let violations: Vec<&Violation> = report.violations.iter().collect();
        assert!(
            violations.is_empty(),
            "sample `{}` (rel={:?}, locale={}) failed C1 linter:\n  body: {:?}\n  violations: {:?}",
            s.file_stem,
            s.relationship,
            s.locale,
            s.body,
            violations
                .iter()
                .map(|v| (&v.rule, &v.detail))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn sample_files_match_in_memory_bodies() {
    // Asserts that the on-disk fixture files in tests/fixtures/voice_quality/
    // exactly match the in-memory `samples()` data. If a dev edits a fixture
    // without updating the array (or vice versa), this fires.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/voice_quality");
    for s in samples() {
        let path = dir.join(format!("{}.md", s.file_stem));
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert!(
            body.contains(s.body),
            "fixture {} does not contain expected body",
            path.display()
        );
    }
}

// ─── Fixture regeneration (run manually) ─────────────────────────────────────

/// Regenerate all 9 fixture `.md` files under `tests/fixtures/voice_quality/`.
///
/// Run with:
/// ```sh
/// cargo test -p mur-agent-runtime --test voice_quality regenerate_fixtures -- --ignored
/// ```
#[test]
#[ignore]
fn regenerate_fixtures() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/voice_quality");
    std::fs::create_dir_all(&dir).unwrap();
    for s in samples() {
        let rel = relationship_segment(&s.relationship);
        let content = format!(
            "---\nrelationship: {rel}\nlocale: {locale}\nsituation: {sit}\nsample: {stem}\n---\n\n{body}\n",
            locale = s.locale,
            sit = s.situation_slug,
            stem = s.file_stem,
            body = s.body,
        );
        let path = dir.join(format!("{}.md", s.file_stem));
        std::fs::write(&path, content).unwrap();
    }
}

fn relationship_segment(r: &Relationship) -> &'static str {
    match r {
        Relationship::Friend => "friend",
        Relationship::Coach => "coach",
        Relationship::AccountabilityBuddy => "accountability_buddy",
        Relationship::Mentor => "mentor",
    }
}
