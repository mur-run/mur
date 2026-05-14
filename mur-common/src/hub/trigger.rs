use std::path::Path;

use serde::{Deserialize, Serialize};

// ─── DwellSpec ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum DwellSpec {
    Seconds(f64),
    UntilDone,
    UntilAck,
    UntilActive,
    Lipsync,
    UntilFocusOff,
}

impl<'de> Deserialize<'de> for DwellSpec {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = DwellSpec;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(
                    f,
                    "a number or one of until_done/until_ack/until_active/lipsync/until_focus_off"
                )
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(DwellSpec::Seconds(v as f64))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(DwellSpec::Seconds(v))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(DwellSpec::Seconds(v as f64))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                match v {
                    "until_done" => Ok(DwellSpec::UntilDone),
                    "until_ack" => Ok(DwellSpec::UntilAck),
                    "until_active" => Ok(DwellSpec::UntilActive),
                    "lipsync" => Ok(DwellSpec::Lipsync),
                    "until_focus_off" => Ok(DwellSpec::UntilFocusOff),
                    _ => Err(E::unknown_variant(
                        v,
                        &[
                            "until_done",
                            "until_ack",
                            "until_active",
                            "lipsync",
                            "until_focus_off",
                        ],
                    )),
                }
            }
        }
        d.deserialize_any(Visitor)
    }
}

impl Serialize for DwellSpec {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            DwellSpec::Seconds(n) => s.serialize_f64(*n),
            DwellSpec::UntilDone => s.serialize_str("until_done"),
            DwellSpec::UntilAck => s.serialize_str("until_ack"),
            DwellSpec::UntilActive => s.serialize_str("until_active"),
            DwellSpec::Lipsync => s.serialize_str("lipsync"),
            DwellSpec::UntilFocusOff => s.serialize_str("until_focus_off"),
        }
    }
}

// ─── ExpressionTrigger ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpressionTrigger {
    /// Event name to match (e.g. "companion.message.new").
    pub on: String,
    /// Expression ID to activate (e.g. "wave").
    pub expression: String,
    pub dwell_s: DwellSpec,
    #[serde(default)]
    pub bubble: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// ─── TriggerSet ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerSet {
    pub triggers: Vec<ExpressionTrigger>,
}

// ─── Loaders ─────────────────────────────────────────────────────────────────

const DEFAULT_YAML: &str = include_str!("triggers/default.yaml");

pub fn default_triggers() -> Vec<ExpressionTrigger> {
    serde_yaml_ng::from_str::<TriggerSet>(DEFAULT_YAML)
        .expect("built-in default.yaml is always valid")
        .triggers
}

/// Load per-agent trigger overrides; falls back to defaults if file is absent or invalid.
pub fn load_triggers(agent_dir: &Path) -> Vec<ExpressionTrigger> {
    let path = agent_dir.join("triggers.yaml");
    if let Ok(text) = std::fs::read_to_string(&path)
        && let Ok(ts) = serde_yaml_ng::from_str::<TriggerSet>(&text)
    {
        return ts.triggers;
    }
    default_triggers()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_triggers_count() {
        assert_eq!(default_triggers().len(), 10);
    }

    #[test]
    fn dwell_spec_numeric() {
        let t: TriggerSet =
            serde_yaml_ng::from_str("triggers:\n  - { on: x, expression: idle, dwell_s: 3.5 }")
                .unwrap();
        assert_eq!(t.triggers[0].dwell_s, DwellSpec::Seconds(3.5));
    }

    #[test]
    fn dwell_spec_until_ack() {
        let t: TriggerSet = serde_yaml_ng::from_str(
            "triggers:\n  - { on: x, expression: error, dwell_s: until_ack }",
        )
        .unwrap();
        assert_eq!(t.triggers[0].dwell_s, DwellSpec::UntilAck);
    }

    #[test]
    fn all_known_dwells_parse() {
        let yaml = "triggers:
  - { on: a, expression: idle, dwell_s: 2 }
  - { on: b, expression: idle, dwell_s: until_done }
  - { on: c, expression: idle, dwell_s: until_ack }
  - { on: d, expression: idle, dwell_s: until_active }
  - { on: e, expression: idle, dwell_s: lipsync }
  - { on: f, expression: idle, dwell_s: until_focus_off }";
        let ts: TriggerSet = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(ts.triggers.len(), 6);
    }
}
