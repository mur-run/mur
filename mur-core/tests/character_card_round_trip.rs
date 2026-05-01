//! M2.7.1 — `MurCard` round-trips an unknown top-level CCv3 field plus the
//! `extensions.mur.first_memory` block. Plan §M2.7.1 step 3.

use mur_core::character_card::schema::*;

#[test]
fn round_trip_unknown_v3_field_preserved() {
    let yaml = r#"
spec: murcard_v1
spec_version: "1.0"
data:
  name: Mochi
extensions:
  mur:
    first_memory:
      text: "Sunday in Taipei"
      established_at: "2026-04-30T14:13:00Z"
unknown_v3_field:
  hello: world
"#;
    let c: MurCard = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(
        c.extensions
            .as_ref()
            .unwrap()
            .mur
            .as_ref()
            .unwrap()
            .first_memory
            .as_ref()
            .unwrap()
            .text,
        "Sunday in Taipei",
    );
    let back = serde_yaml_ng::to_string(&c).unwrap();
    assert!(
        back.contains("unknown_v3_field"),
        "passthrough lost the unknown top-level key:\n{back}"
    );
    assert!(
        back.contains("hello: world"),
        "passthrough lost the nested value:\n{back}"
    );
}
