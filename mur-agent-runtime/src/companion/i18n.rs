//! Locale-mismatch detection. Used to decide whether to trigger the translate fallback
//! when LLM output's apparent language doesn't match the user's `companion.locale`.
//!
//! Spec §3.6 / §4.4 / §6.2.

use crate::llm::{LlmClient, LlmError, LlmRequest, RichMessage};

// ──────────────────────────────────────────────────────────────────────────────
// EnsureLocaleOutcome
// ──────────────────────────────────────────────────────────────────────────────

/// Result of [`ensure_locale`].
///
/// Spec §4.4.
#[derive(Debug)]
pub enum EnsureLocaleOutcome {
    /// `heuristic_matches` said the text is already in the target locale — no
    /// translation needed.
    Original,
    /// LLM translation succeeded; caller should ship `new_body` to the notifier.
    Translated(String),
    /// Reactive path: translate failed, ship the original body anyway and log
    /// the error.
    OriginalWithLog(LlmError),
    /// Proactive path: translate failed; caller should pause + schedule a retry.
    QueuedRetry(LlmError),
}

// ──────────────────────────────────────────────────────────────────────────────
// ensure_locale
// ──────────────────────────────────────────────────────────────────────────────

/// Verify (or translate) `text` into `target_locale`.
///
/// 1. If `heuristic_matches(text, target_locale)` → return [`EnsureLocaleOutcome::Original`].
/// 2. Otherwise call the LLM translate prompt.
/// 3. On LLM Ok with non-empty trimmed result → `Translated(result)`.
/// 4. On LLM Ok with empty trimmed result → treat as `InvalidResponse("empty
///    translation")` and fall through to error handling.
/// 5. On LLM Err:
///    - `reactive = true`  → `OriginalWithLog(err)`
///    - `reactive = false` → `QueuedRetry(err)`
pub async fn ensure_locale(
    text: &str,
    target_locale: &str,
    llm: &dyn LlmClient,
    reactive: bool,
) -> EnsureLocaleOutcome {
    if heuristic_matches(text, target_locale) {
        return EnsureLocaleOutcome::Original;
    }

    let req = LlmRequest {
        messages: vec![
            RichMessage::Text {
                role: "system".to_string(),
                content: format!(
                    "Translate the following message into {target_locale} while preserving \
                     tone and brevity. Reply ONLY with the translated message, no preamble."
                ),
            },
            RichMessage::Text {
                role: "user".to_string(),
                content: text.to_string(),
            },
        ],
        temperature: Some(0.2),
        max_tokens: Some(400),
        tools: vec![],
        ..Default::default()
    };

    match llm.generate(req).await {
        Ok(resp) => {
            let trimmed = resp.text.trim().to_string();
            if trimmed.is_empty() {
                let err = LlmError::InvalidResponse("empty translation".to_string());
                if reactive {
                    EnsureLocaleOutcome::OriginalWithLog(err)
                } else {
                    EnsureLocaleOutcome::QueuedRetry(err)
                }
            } else {
                EnsureLocaleOutcome::Translated(trimmed)
            }
        }
        Err(err) => {
            if reactive {
                EnsureLocaleOutcome::OriginalWithLog(err)
            } else {
                EnsureLocaleOutcome::QueuedRetry(err)
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// heuristic_matches
// ──────────────────────────────────────────────────────────────────────────────

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
        "deu" => "de",
        "eng" => "en",
        "fra" => "fr",
        "spa" => "es",
        "ita" => "it",
        "por" => "pt",
        "rus" => "ru",
        "nld" => "nl",
        "pol" => "pl",
        "tur" => "tr",
        "ukr" => "uk",
        "vie" => "vi",
        "swe" => "sv",
        "fin" => "fi",
        "dan" => "da",
        "nor" => "no",
        "ell" => "el",
        "ces" => "cs",
        "ron" => "ro",
        "hun" => "hu",
        "ind" => "id",
        "tha" => "th",
        "ara" => "ar",
        "heb" => "he",
        "jpn" => "ja",
        "zho" | "cmn" => "zh",
        "kor" => "ko",
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

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::stub::StubLlm;
    use std::sync::Arc;

    fn zh_tw_body() -> &'static str {
        "早安！今天想從哪一件小事開始？"
    }

    fn en_body() -> &'static str {
        "Good morning! What small thing will you start with today?"
    }

    // ── Test 1: body is already zh-TW → Original, no LLM call ────────────────

    #[tokio::test]
    async fn match_locale_returns_original() {
        // zh-TW body should satisfy zh heuristic (CJK ratio ≥ 0.30).
        let stub = Arc::new(
            StubLlm::from_yaml(
                r#"
- match:
    contains: "translate"
  fault: rate_limit
"#,
            )
            .unwrap(),
        );

        let outcome = ensure_locale(zh_tw_body(), "zh-TW", stub.as_ref(), false).await;
        assert!(
            matches!(outcome, EnsureLocaleOutcome::Original),
            "zh-TW body against zh-TW locale should be Original; got {:?}",
            match outcome {
                EnsureLocaleOutcome::Original => "Original",
                EnsureLocaleOutcome::Translated(_) => "Translated",
                EnsureLocaleOutcome::OriginalWithLog(_) => "OriginalWithLog",
                EnsureLocaleOutcome::QueuedRetry(_) => "QueuedRetry",
            }
        );
    }

    // ── Test 2: English body against zh-TW → stub translates → Translated ────

    #[tokio::test]
    async fn mismatch_translates_successfully() {
        let translated_body = "早安！今天想從哪一件小事開始？";
        let stub = Arc::new(
            StubLlm::from_yaml(&format!(
                r#"
- match:
    contains: "Good morning"
  response: "{translated_body}"
"#
            ))
            .unwrap(),
        );

        let outcome = ensure_locale(en_body(), "zh-TW", stub.as_ref(), false).await;
        match outcome {
            EnsureLocaleOutcome::Translated(text) => {
                assert_eq!(
                    text, translated_body,
                    "translated text must match stub response"
                );
            }
            other => panic!(
                "expected Translated; got {:?}",
                match other {
                    EnsureLocaleOutcome::Original => "Original",
                    EnsureLocaleOutcome::Translated(_) => "Translated",
                    EnsureLocaleOutcome::OriginalWithLog(_) => "OriginalWithLog",
                    EnsureLocaleOutcome::QueuedRetry(_) => "QueuedRetry",
                }
            ),
        }
    }

    // ── Test 3: English body, proactive, stub forces RateLimit → QueuedRetry ─

    #[tokio::test]
    async fn mismatch_proactive_failure_queues() {
        let stub = Arc::new(
            StubLlm::from_yaml(
                r#"
- match:
    contains: "Good morning"
  fault: rate_limit
"#,
            )
            .unwrap(),
        );

        let outcome = ensure_locale(en_body(), "zh-TW", stub.as_ref(), false).await;
        assert!(
            matches!(outcome, EnsureLocaleOutcome::QueuedRetry(_)),
            "proactive RateLimit should yield QueuedRetry"
        );
    }

    // ── Test 4 (optional): reactive=true, stub RateLimit → OriginalWithLog ───

    #[tokio::test]
    async fn mismatch_reactive_failure_ships_original() {
        let stub = Arc::new(
            StubLlm::from_yaml(
                r#"
- match:
    contains: "Good morning"
  fault: rate_limit
"#,
            )
            .unwrap(),
        );

        let outcome = ensure_locale(en_body(), "zh-TW", stub.as_ref(), true).await;
        assert!(
            matches!(outcome, EnsureLocaleOutcome::OriginalWithLog(_)),
            "reactive RateLimit should yield OriginalWithLog"
        );
    }
}
