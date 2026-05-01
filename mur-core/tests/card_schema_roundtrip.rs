//! M4.1.1 — Full CCv3 `CardData` with `Asset` and `CharacterBook`.
//! Tests both minimal (backwards-compatible) and full CCv3 round-tripping.

use mur_core::character_card::schema::*;

#[test]
fn full_ccv3_data_round_trip_yaml() {
    let yaml = r#"
spec: murcard_v1
spec_version: "1.0"
data:
  name: Aiko
  nickname: Ai
  description: A patient programming companion.
  personality: "warm, precise, curious"
  scenario: late-night pair programming session
  first_mes: "Hey — what are we building tonight?"
  mes_example: "<START>\n{{user}}: hi\n{{char}}: hi back"
  alternate_greetings:
    - "yo"
  system_prompt: ""
  post_history_instructions: ""
  creator: "did:mur:z6Mk..."
  creator_notes_multilingual:
    en: ""
    zh-TW: ""
  character_version: "1.0.0"
  creation_date: 1761868800
  modification_date: 1761868800
  tags: [companion, coding]
  source:
    - "https://chub.ai/characters/foo"
  assets:
    - { type: icon, uri: "embeded://avatar.png", name: main, ext: png }
  character_book:
    name: "Aiko's world"
    scan_depth: 4
    token_budget: 512
    recursive_scanning: false
    entries:
      - keys: [rust, cargo]
        content: "Aiko prefers idiomatic Rust 2024…"
        enabled: true
        insertion_order: 100
        position: before_char
        constant: false
"#;
    let card: MurCard = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(card.data.name, "Aiko");
    assert_eq!(card.data.nickname.as_deref(), Some("Ai"));
    assert_eq!(card.data.alternate_greetings.len(), 1);
    assert_eq!(card.data.tags.len(), 2);
    let book = card.data.character_book.as_ref().unwrap();
    assert_eq!(book.entries[0].keys, vec!["rust", "cargo"]);
    let back = serde_yaml_ng::to_string(&card).unwrap();
    assert!(back.contains("nickname: Ai"));
}

#[test]
fn minimal_card_still_round_trips() {
    let yaml = "spec: murcard_v1\nspec_version: \"1.0\"\ndata:\n  name: Mochi\n";
    let card: MurCard = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(card.data.name, "Mochi");
    assert!(card.data.tags.is_empty());
}
