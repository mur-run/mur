use mur_core::cmd::agent_companion::card::cai::normalize_cai;

#[test]
fn cai_aiko_maps_correctly() {
    let json = include_str!("fixtures/cards/cai-aiko.json");
    let card = normalize_cai(json).unwrap();
    assert_eq!(card.data.name, "Aiko");
    assert!(card.data.description.contains("tokio debugging"));
    assert_eq!(card.data.first_mes, "Hey — what are we building tonight?");
    let mur = card.extensions.as_ref().unwrap().mur.as_ref().unwrap();
    let voice = mur.voice.as_ref().unwrap();
    assert_eq!(voice.provider, "character-ai");
    assert_eq!(voice.voice_id, "voice-9876");
    assert!(card.data.source.iter().any(|s| s.contains("character.ai")));
}

#[test]
fn cai_without_voice_uses_provider_none() {
    let json = r#"{"name":"NoVoice","definition":"x","greeting":"hi"}"#;
    let card = normalize_cai(json).unwrap();
    let voice = card.extensions.as_ref().unwrap().mur.as_ref().unwrap()
        .voice.as_ref().expect("voice block always present");
    assert_eq!(voice.provider, "none");
    assert!(voice.voice_id.is_empty());
}
