//! Voice template lookup + locale fallback (M2.1, spec §4.3).

use mur_common::companion::{Relationship, voice_template};

#[test]
fn exact_match_friend_zh_tw() {
    let (used, body) = voice_template::resolve_locale(&Relationship::Friend, "zh-TW");
    assert_eq!(used, "zh-TW");
    assert!(body.contains("{{NAME_FOR_USER}}"));
}

#[test]
fn exact_match_mentor_zh_tw() {
    let (used, _) = voice_template::resolve_locale(&Relationship::Mentor, "zh-TW");
    assert_eq!(used, "zh-TW");
}

#[test]
fn fallback_to_en_for_unknown_locale() {
    // mentor de-DE: no exact, no "de" → fall back to en-US.
    let (used, _) = voice_template::resolve_locale(&Relationship::Mentor, "de-DE");
    assert_eq!(used, "en-US");
}

#[test]
fn fallback_zh_cn_for_mentor_uses_en() {
    // mentor zh-CN: no exact (only friend has zh-CN), no plain "zh" → en-US.
    let (used, _) = voice_template::resolve_locale(&Relationship::Mentor, "zh-CN");
    assert_eq!(used, "en-US");
}

#[test]
fn all_templates_use_only_known_placeholders() {
    let allowed = [
        "{{NAME_FOR_USER}}",
        "{{FORMALITY}}",
        "{{EXTRA_INSTRUCTIONS}}",
        "{{LOCALE}}",
    ];
    for r in [
        Relationship::Friend,
        Relationship::Coach,
        Relationship::AccountabilityBuddy,
        Relationship::Mentor,
    ] {
        for loc in ["zh-TW", "en-US", "zh-CN", "ja-JP"] {
            if let Some(body) = voice_template::embedded(&r, loc) {
                let mut idx = 0;
                while let Some(start) = body[idx..].find("{{") {
                    let abs_start = idx + start;
                    let end =
                        body[abs_start..].find("}}").expect("unterminated {{") + abs_start + 2;
                    let placeholder = &body[abs_start..end];
                    assert!(
                        allowed.contains(&placeholder),
                        "unknown placeholder {placeholder} in {r:?}.{loc}"
                    );
                    idx = end;
                }
            }
        }
    }
}
