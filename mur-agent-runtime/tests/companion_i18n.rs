use mur_agent_runtime::companion::i18n::heuristic_matches;

#[test]
fn cjk_block_detects_zh() {
    assert!(heuristic_matches("早安 David。今天好嗎？", "zh-TW"));
    assert!(heuristic_matches("早安 David。今天好嗎？", "zh-CN"));
    assert!(!heuristic_matches("Good morning David.", "zh-TW"));
}

#[test]
fn english_target_always_matches() {
    assert!(heuristic_matches("anything goes", "en-US"));
    assert!(heuristic_matches("早安 David", "en-US")); // even Chinese text "matches" en-US
}

#[test]
fn japanese_kana_detects_ja() {
    assert!(heuristic_matches("おはようございます。", "ja-JP"));
    assert!(!heuristic_matches("Good morning.", "ja-JP"));
}

#[test]
fn korean_hangul_detects_ko() {
    assert!(heuristic_matches("안녕하세요.", "ko-KR"));
    assert!(!heuristic_matches("Hello.", "ko-KR"));
}

#[test]
fn whatlang_for_german() {
    // whatlang correctly identifies German
    assert!(heuristic_matches(
        "Guten Morgen, wie geht es dir heute?",
        "de-DE"
    ));
    // English text against de-DE target should return false (whatlang detects English)
    assert!(!heuristic_matches(
        "Good morning, how are you today?",
        "de-DE"
    ));
}

#[test]
fn unknown_locale_or_empty_text_conservative() {
    // Empty / very short text — whatlang likely returns None — conservative true
    assert!(heuristic_matches("", "fr-FR"));
    assert!(heuristic_matches("ok", "vi-VN"));
}
