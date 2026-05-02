use mur_core::character_card::canonical::canonical_data_bytes;
use mur_core::character_card::schema::{CardData, MurCard};

fn minimal_card() -> MurCard {
    MurCard {
        spec: "murcard_v1".into(),
        spec_version: "1.0".into(),
        data: CardData {
            name: "Aiko".into(),
            ..Default::default()
        },
        extensions: None,
        ccv3_passthrough: Default::default(),
    }
}

#[test]
fn canonical_is_deterministic() {
    let c = minimal_card();
    let a = canonical_data_bytes(&c).unwrap();
    let b = canonical_data_bytes(&c).unwrap();
    assert_eq!(a, b);
}

#[test]
fn canonical_excludes_signature_block() {
    use mur_core::character_card::extensions::*;
    use mur_core::character_card::schema::Extensions;

    // Build a card with provenance.content_rating set but no signature.
    let mut unsigned_card = minimal_card();
    unsigned_card.extensions = Some(Extensions {
        mur: Some(MurExt {
            schema_version: 1,
            voice: None,
            avatar: None,
            relationship: None,
            first_memory: None,
            companion: None,
            provenance: Some(ProvenanceExt {
                signature: None,
                content_rating: Some("sfw".into()),
                import_trust: None,
            }),
        }),
    });
    let unsigned = canonical_data_bytes(&unsigned_card).unwrap();

    // Same card but with a signature attached.
    let mut signed_card = unsigned_card.clone();
    signed_card
        .extensions
        .as_mut()
        .unwrap()
        .mur
        .as_mut()
        .unwrap()
        .provenance
        .as_mut()
        .unwrap()
        .signature = Some(CardSignature {
        algorithm: "ed25519".into(),
        public_key: "z6MkABC".into(),
        value: "z3sigDEF".into(),
        signed_at: chrono::Utc::now(),
    });
    let signed = canonical_data_bytes(&signed_card).unwrap();
    assert_eq!(
        signed, unsigned,
        "signature block must NOT influence canonical bytes",
    );
}

#[test]
fn canonical_array_order_preserved() {
    // Arrays preserve author order — different tag order produces
    // different canonical bytes.
    let mut c1 = minimal_card();
    c1.data.tags = vec!["a".into(), "b".into()];
    let mut c2 = minimal_card();
    c2.data.tags = vec!["b".into(), "a".into()];
    let b1 = canonical_data_bytes(&c1).unwrap();
    let b2 = canonical_data_bytes(&c2).unwrap();
    assert_ne!(b1, b2);
}

#[test]
fn sign_then_verify_round_trip() {
    use ed25519_dalek::SigningKey;
    use mur_core::character_card::signing::{VerifyOutcome, sign_card, verify_card};
    use rand::rngs::OsRng;

    let mut card = minimal_card();
    let sk = SigningKey::generate(&mut OsRng);
    sign_card(&mut card, &sk).unwrap();

    let sig = card
        .extensions
        .as_ref()
        .unwrap()
        .mur
        .as_ref()
        .unwrap()
        .provenance
        .as_ref()
        .unwrap()
        .signature
        .as_ref()
        .unwrap();
    assert_eq!(sig.algorithm, "ed25519");
    assert!(sig.public_key.starts_with('z'));
    assert!(sig.value.starts_with('z'));

    let outcome = verify_card(&card).expect("freshly-signed card verifies");
    assert!(matches!(outcome, VerifyOutcome::Signed));
}

#[test]
fn tampered_card_fails_verification() {
    use ed25519_dalek::SigningKey;
    use mur_core::character_card::signing::{sign_card, verify_card};
    use rand::rngs::OsRng;

    let mut card = minimal_card();
    let sk = SigningKey::generate(&mut OsRng);
    sign_card(&mut card, &sk).unwrap();
    card.data.description = "evil instructions".into();
    assert!(verify_card(&card).is_err(), "tampered card must fail");
}

#[test]
fn unsigned_card_returns_unsigned() {
    use mur_core::character_card::signing::{VerifyOutcome, verify_card};
    let card = minimal_card();
    let outcome = verify_card(&card).expect("missing-sig is not an error");
    assert!(matches!(outcome, VerifyOutcome::Unsigned));
}
