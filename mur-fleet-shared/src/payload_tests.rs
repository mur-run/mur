use crate::payload::*;

pub(crate) const ID: &str = "0123456789abcdef0123456789abcdef";
pub(crate) const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

pub(crate) fn payload() -> RefPayload {
    RefPayload {
        fleet: "f".into(),
        run: "r1".into(),
        member: "alice".into(),
        name: "notes.md".into(),
        seq: 3,
        sha256: SHA.into(),
        size: 42,
        admitted_at: 1_700_000_000,
        request_id: ID.into(),
        key_version: 0,
    }
}

fn roundtrip<T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(
    v: T,
) {
    let json = serde_json::to_string(&v).unwrap();
    assert_eq!(serde_json::from_str::<T>(&json).unwrap(), v, "{json}");
}

#[test]
fn ref_payload_round_trips_and_carries_v_and_kind() {
    let p = payload();
    roundtrip(p.clone());
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v["v"], 1);
    assert_eq!(v["kind"], "fleet_shared_ref");
    assert_eq!(v["request_id"], ID);
    assert_eq!(v["admitted_at"], 1_700_000_000_i64);
}

#[test]
fn wire_ref_round_trips_with_ref_key() {
    let w = WireRef {
        payload: payload(),
        sig: "zSig".into(),
    };
    roundtrip(w.clone());
    let v = serde_json::to_value(&w).unwrap();
    assert!(v.get("ref").is_some() && v.get("sig").is_some());
}

#[test]
fn admission_record_round_trips() {
    roundtrip(AdmissionRecord {
        fleet: "f".into(),
        run: "r1".into(),
        member: "alice".into(),
        name: "notes.md".into(),
        seq: 1,
        sha256: SHA.into(),
        size: u64::MAX,
        admitted_at: -5,
        request_id: ID.into(),
    });
}

#[test]
fn index_round_trips_both_states_in_the_spec_shape() {
    let mut idx = Index::default();
    idx.0.insert(
        "a".into(),
        IndexEntry {
            seq: 1,
            sha256: SHA.into(),
            state: EntryState::Live,
        },
    );
    idx.0.insert(
        "b".into(),
        IndexEntry {
            seq: 9,
            sha256: SHA.into(),
            state: EntryState::Tombstone,
        },
    );
    roundtrip(idx.clone());
    let v = serde_json::to_value(&idx).unwrap();
    assert_eq!(v["a"]["state"], "live");
    assert_eq!(v["b"]["state"], "tombstone");
}

#[test]
fn seq_zero_and_bad_request_id_do_not_parse() {
    let mut v = serde_json::to_value(payload()).unwrap();
    v["seq"] = 0.into();
    assert!(serde_json::from_value::<RefPayload>(v).is_err());
    let mut v = serde_json::to_value(payload()).unwrap();
    v["request_id"] = "ABC".into();
    assert!(serde_json::from_value::<RefPayload>(v).is_err());
    let entry = r#"{"seq":0,"sha256":"x","state":"live"}"#;
    assert!(serde_json::from_str::<IndexEntry>(entry).is_err());
}

#[test]
fn wrong_v_wrong_kind_and_unknown_field_do_not_parse() {
    for (key, val) in [
        ("v", serde_json::json!(2)),
        ("kind", "channel".into()),
        ("extra", 1.into()),
    ] {
        let mut v = serde_json::to_value(payload()).unwrap();
        v[key] = val;
        assert!(serde_json::from_value::<RefPayload>(v).is_err(), "{key}");
    }
}
