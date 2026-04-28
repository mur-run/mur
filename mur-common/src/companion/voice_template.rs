//! Embedded voice templates with locale lookup chain.
//!
//! The (relationship × locale) matrix lives in `templates/`. Lookup falls
//! back from exact match → language-only match → `en-US` (always present).
//! See spec §4.3 for the full strategy.

use crate::companion::Relationship;

/// Returns the embedded template body for the (relationship, locale) pair, or
/// `None` if the exact pair is not embedded. Use [`resolve_locale`] for the
/// fallback-aware lookup.
pub fn embedded(relationship: &Relationship, locale: &str) -> Option<&'static str> {
    match (relationship, locale) {
        (Relationship::Friend, "zh-TW") => Some(include_str!("templates/friend.zh-TW.md")),
        (Relationship::Friend, "en-US") => Some(include_str!("templates/friend.en-US.md")),
        (Relationship::Friend, "zh-CN") => Some(include_str!("templates/friend.zh-CN.md")),
        (Relationship::Friend, "ja-JP") => Some(include_str!("templates/friend.ja-JP.md")),
        (Relationship::Coach, "zh-TW") => Some(include_str!("templates/coach.zh-TW.md")),
        (Relationship::Coach, "en-US") => Some(include_str!("templates/coach.en-US.md")),
        (Relationship::AccountabilityBuddy, "zh-TW") => {
            Some(include_str!("templates/accountability_buddy.zh-TW.md"))
        }
        (Relationship::AccountabilityBuddy, "en-US") => {
            Some(include_str!("templates/accountability_buddy.en-US.md"))
        }
        (Relationship::Mentor, "zh-TW") => Some(include_str!("templates/mentor.zh-TW.md")),
        (Relationship::Mentor, "en-US") => Some(include_str!("templates/mentor.en-US.md")),
        _ => None,
    }
}

/// Locale fallback chain: exact → language-only → en-US.
/// Returns (locale_actually_used, template_body).
pub fn resolve_locale(relationship: &Relationship, locale: &str) -> (String, &'static str) {
    if let Some(t) = embedded(relationship, locale) {
        return (locale.into(), t);
    }
    let lang = locale.split('-').next().unwrap_or(locale);
    if locale != lang
        && let Some(t) = embedded(relationship, lang)
    {
        return (lang.into(), t);
    }
    let t = embedded(relationship, "en-US")
        .expect("en-US template must exist for every relationship");
    ("en-US".into(), t)
}
