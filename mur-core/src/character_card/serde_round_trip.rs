//! Round-trip helpers for `MurCard`.
//!
//! The CCv3 passthrough is implemented directly on `schema::MurCard` via
//! `#[serde(flatten)] BTreeMap<String, serde_yaml_ng::Value>`, so this
//! module is currently a placeholder. Future helpers (e.g. canonical
//! field-order writers, schema-version migrators) will land here.
