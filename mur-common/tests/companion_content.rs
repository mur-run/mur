use mur_common::companion::content_seed::{MORNING_GREETING_EN_US, MORNING_GREETING_ZH_TW};

#[test]
fn morning_greeting_has_first_memory_template_en() {
    assert!(
        MORNING_GREETING_EN_US.contains("{{FIRST_MEMORY}}"),
        "expected at least one en-US morning_greeting template referencing {{{{FIRST_MEMORY}}}}: {MORNING_GREETING_EN_US}",
    );
}

#[test]
fn morning_greeting_has_first_memory_template_zh_tw() {
    assert!(
        MORNING_GREETING_ZH_TW.contains("{{FIRST_MEMORY}}"),
        "expected at least one zh-TW morning_greeting template referencing {{{{FIRST_MEMORY}}}}: {MORNING_GREETING_ZH_TW}",
    );
}
