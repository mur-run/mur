use mur_core::cmd::agent_companion::card::png::{extract_card_json, normalize_v2};

#[test]
fn v2_normalizer_wraps_into_murcard() {
    let png = include_bytes!("fixtures/cards/silly-v2.png");
    let json = extract_card_json(png).unwrap();
    let card = normalize_v2(&json).unwrap();
    assert_eq!(card.spec, "murcard_v1");
    assert_eq!(card.data.name, "TestV2");
    assert_eq!(card.data.description, "v2 imported");
}
