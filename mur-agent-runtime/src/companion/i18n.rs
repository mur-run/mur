//! Locale-mismatch detection. Used to decide whether to trigger the translate fallback
//! when LLM output's apparent language doesn't match the user's `companion.locale`.
//!
//! Spec §3.6 / §6.2.

/// Returns true if `text` plausibly is in `target_locale`'s primary language.
/// Conservative: returns true for unknown locales or unparsable text (avoids
/// false-triggering the translate path).
pub fn heuristic_matches(text: &str, target_locale: &str) -> bool {
    if target_locale.starts_with("en") {
        return true;
    }
    if target_locale.starts_with("zh") {
        return cjk_ratio(text) >= 0.30;
    }
    if target_locale.starts_with("ja") {
        return ja_ratio(text) >= 0.20;
    }
    if target_locale.starts_with("ko") {
        return hangul_ratio(text) >= 0.30;
    }

    // Latin / other scripts → whatlang
    // Conservative: very short text yields unreliable detection — treat as match.
    if text.chars().filter(|c| !c.is_whitespace()).count() < 8 {
        return true;
    }
    match whatlang::detect(text) {
        Some(info) => target_locale.starts_with(iso639_1_for(info.lang().code())),
        None => true, // conservative
    }
}

/// Map whatlang's ISO 639-3 code to the ISO 639-1 prefix used in BCP-47 locales.
/// Returns the input unchanged when unknown (best-effort prefix match).
fn iso639_1_for(code_3: &str) -> &str {
    match code_3 {
        "deu" => "de", "eng" => "en", "fra" => "fr", "spa" => "es",
        "ita" => "it", "por" => "pt", "rus" => "ru", "nld" => "nl",
        "pol" => "pl", "tur" => "tr", "ukr" => "uk", "vie" => "vi",
        "swe" => "sv", "fin" => "fi", "dan" => "da", "nor" => "no",
        "ell" => "el", "ces" => "cs", "ron" => "ro", "hun" => "hu",
        "ind" => "id", "tha" => "th", "ara" => "ar", "heb" => "he",
        "jpn" => "ja", "zho" | "cmn" => "zh", "kor" => "ko",
        _ => code_3,
    }
}

fn ratio(s: &str, f: impl Fn(char) -> bool) -> f32 {
    let total = s.chars().filter(|c| !c.is_whitespace()).count();
    if total == 0 {
        return 0.0;
    }
    let hits = s.chars().filter(|&c| f(c)).count();
    hits as f32 / total as f32
}

fn cjk_ratio(s: &str) -> f32 {
    ratio(s, |c| matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF))
}
fn ja_ratio(s: &str) -> f32 {
    // Hiragana (U+3040..U+309F) and Katakana (U+30A0..U+30FF) are contiguous.
    ratio(s, |c| matches!(c as u32, 0x3040..=0x30FF))
}
fn hangul_ratio(s: &str) -> f32 {
    ratio(s, |c| matches!(c as u32, 0xAC00..=0xD7AF))
}
