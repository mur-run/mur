use mur_core::cmd::agent_companion::card::png::extract_card_json;

#[test]
fn extract_v3_chunk() {
    let png = include_bytes!("fixtures/cards/silly-v3.png");
    let json = extract_card_json(png).expect("v3 chunk present");
    assert!(json.contains("\"spec\""));
    assert!(json.contains("character_book"));
}

#[test]
fn extract_v2_chunk_falls_back_when_no_v3() {
    let png = include_bytes!("fixtures/cards/silly-v2.png");
    let json = extract_card_json(png).expect("v2 chunk present");
    // V2 is flat top-level JSON: name + description + ...
    assert!(json.contains("\"name\""));
}

#[test]
fn no_text_chunks_errors() {
    // A plain PNG with no chara/ccv3 — just IHDR + IDAT + IEND.
    let plain = build_plain_png_no_chunks();
    let err = extract_card_json(&plain).expect_err("no chara/ccv3");
    let msg = err.to_string();
    assert!(msg.contains("chara") || msg.contains("ccv3"), "got: {msg}");
}

#[test]
fn v3_normalizer_preserves_data() {
    use mur_core::cmd::agent_companion::card::png::{extract_card_json, normalize_v3};
    let png = include_bytes!("fixtures/cards/silly-v3.png");
    let json = extract_card_json(png).unwrap();
    let card = normalize_v3(&json).unwrap();
    assert_eq!(card.data.name, "TestV3");
    assert_eq!(card.data.description, "imported via fixture");
    assert_eq!(card.spec, "murcard_v1");
    assert_eq!(card.spec_version, "1.0");
    let book = card
        .data
        .character_book
        .as_ref()
        .expect("character_book preserved");
    assert_eq!(book.name, "x");
}

fn build_plain_png_no_chunks() -> Vec<u8> {
    // Reuses the M3.2.3 1x1 PNG — known good 67-byte transparent.
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ]
}
