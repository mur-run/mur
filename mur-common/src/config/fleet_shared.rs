//! Fleet shared area limits (`fleet.shared_area:`).
//!
//! Every value here is a limit, so a bad entry must never relax one. The
//! block is therefore read leniently, key by key: a value that is not a
//! positive integer, an unknown key, or a key that belongs to a later version
//! falls back to the default for that key alone and is recorded in
//! [`FleetSharedAreaConfig::problems`], instead of failing the parse (which
//! would make [`Config::load_or_default`] drop the user's whole config).

use serde::{Deserialize, Deserializer, Serialize};

/// Days an admitted object stays readable.
pub const FLEET_SHARED_DEFAULT_RETENTION_DAYS: u64 = 7;
/// Largest single object, in KiB.
pub const FLEET_SHARED_DEFAULT_MAX_OBJECT_KB: u64 = 128;
/// Live names one member may hold in one fleet.
pub const FLEET_SHARED_DEFAULT_MAX_MEMBER_OBJECTS: u64 = 256;
/// Bytes one member may hold in one fleet, in MiB.
pub const FLEET_SHARED_DEFAULT_MAX_MEMBER_MB: u64 = 32;
/// Bytes one fleet may hold, in MiB.
pub const FLEET_SHARED_DEFAULT_MAX_FLEET_MB: u64 = 256;
/// Bytes the whole store may hold, in MiB.
pub const FLEET_SHARED_DEFAULT_MAX_TOTAL_MB: u64 = 1024;
/// Seconds a runtime waits for the daemon's reply.
pub const FLEET_SHARED_DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;
/// Seconds between daemon sweeps and between runtime reply polls.
pub const FLEET_SHARED_DEFAULT_POLL_SECS: u64 = 1;

/// Keys that exist only in a later version; a v1 config that sets one is
/// told so and the key has no effect.
pub const FLEET_SHARED_V2_KEYS: &[&str] = &["max_file_mb", "staging_grace_hours"];

const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;

/// What happens to an object past `retention_days`. v1 has one choice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetSharedOnExpiry {
    #[default]
    Delete,
}

/// One entry of `fleet.shared_area:` that was not applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetSharedConfigProblem {
    /// The block itself is not a mapping; every key is at its default.
    NotAMapping,
    /// A key of a later version (see [`FLEET_SHARED_V2_KEYS`]).
    V2Key(String),
    /// A key this version does not know.
    UnknownKey(String),
    /// A known key whose value is not usable; its default applies.
    InvalidValue(String),
}

impl std::fmt::Display for FleetSharedConfigProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAMapping => {
                write!(f, "fleet.shared_area is not a mapping; defaults apply")
            }
            Self::V2Key(k) => write!(
                f,
                "fleet.shared_area.{k} is not available in this version; it is ignored"
            ),
            Self::UnknownKey(k) => write!(f, "fleet.shared_area.{k} is unknown; it is ignored"),
            Self::InvalidValue(k) => write!(
                f,
                "fleet.shared_area.{k} is not a valid value; the default applies"
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FleetSharedAreaConfig {
    pub retention_days: u64,
    pub max_object_kb: u64,
    pub max_member_objects: u64,
    pub max_member_mb: u64,
    pub max_fleet_mb: u64,
    pub max_total_mb: u64,
    pub on_expiry: FleetSharedOnExpiry,
    /// Read it through [`FleetSharedAreaConfig::effective_timeout_secs`].
    pub request_timeout_secs: u64,
    pub poll_secs: u64,
    /// Entries of the block that were not applied. Never written back.
    #[serde(skip)]
    pub problems: Vec<FleetSharedConfigProblem>,
}

impl Default for FleetSharedAreaConfig {
    fn default() -> Self {
        Self {
            retention_days: FLEET_SHARED_DEFAULT_RETENTION_DAYS,
            max_object_kb: FLEET_SHARED_DEFAULT_MAX_OBJECT_KB,
            max_member_objects: FLEET_SHARED_DEFAULT_MAX_MEMBER_OBJECTS,
            max_member_mb: FLEET_SHARED_DEFAULT_MAX_MEMBER_MB,
            max_fleet_mb: FLEET_SHARED_DEFAULT_MAX_FLEET_MB,
            max_total_mb: FLEET_SHARED_DEFAULT_MAX_TOTAL_MB,
            on_expiry: FleetSharedOnExpiry::default(),
            request_timeout_secs: FLEET_SHARED_DEFAULT_REQUEST_TIMEOUT_SECS,
            poll_secs: FLEET_SHARED_DEFAULT_POLL_SECS,
            problems: Vec::new(),
        }
    }
}

/// The limits decide equality; `problems` only describes how they were read.
impl PartialEq for FleetSharedAreaConfig {
    fn eq(&self, other: &Self) -> bool {
        self.retention_days == other.retention_days
            && self.max_object_kb == other.max_object_kb
            && self.max_member_objects == other.max_member_objects
            && self.max_member_mb == other.max_member_mb
            && self.max_fleet_mb == other.max_fleet_mb
            && self.max_total_mb == other.max_total_mb
            && self.on_expiry == other.on_expiry
            && self.request_timeout_secs == other.request_timeout_secs
            && self.poll_secs == other.poll_secs
    }
}

impl FleetSharedAreaConfig {
    /// True when every limit is at its default, so the block need not be
    /// written to `config.yaml`.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn max_object_bytes(&self) -> u64 {
        self.max_object_kb.saturating_mul(KIB)
    }

    pub fn max_member_bytes(&self) -> u64 {
        self.max_member_mb.saturating_mul(MIB)
    }

    pub fn max_fleet_bytes(&self) -> u64 {
        self.max_fleet_mb.saturating_mul(MIB)
    }

    pub fn max_total_bytes(&self) -> u64 {
        self.max_total_mb.saturating_mul(MIB)
    }

    /// `request_timeout_secs` rounded up to a whole number of polls, so the
    /// last poll never falls after the deadline.
    pub fn effective_timeout_secs(&self) -> u64 {
        self.request_timeout_secs
            .div_ceil(self.poll_secs)
            .saturating_mul(self.poll_secs)
    }
}

impl<'de> Deserialize<'de> for FleetSharedAreaConfig {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut cfg = Self::default();
        let serde_json::Value::Object(map) = serde_json::Value::deserialize(d)? else {
            cfg.problems.push(FleetSharedConfigProblem::NotAMapping);
            return Ok(cfg);
        };
        for (key, value) in map {
            let applied = match key.as_str() {
                "retention_days" => positive(&value, &mut cfg.retention_days),
                "max_object_kb" => positive(&value, &mut cfg.max_object_kb),
                "max_member_objects" => positive(&value, &mut cfg.max_member_objects),
                "max_member_mb" => positive(&value, &mut cfg.max_member_mb),
                "max_fleet_mb" => positive(&value, &mut cfg.max_fleet_mb),
                "max_total_mb" => positive(&value, &mut cfg.max_total_mb),
                "request_timeout_secs" => positive(&value, &mut cfg.request_timeout_secs),
                "poll_secs" => positive(&value, &mut cfg.poll_secs),
                "on_expiry" => match serde_json::from_value(value) {
                    Ok(v) => {
                        cfg.on_expiry = v;
                        true
                    }
                    Err(_) => false,
                },
                k if FLEET_SHARED_V2_KEYS.contains(&k) => {
                    cfg.problems.push(FleetSharedConfigProblem::V2Key(key));
                    continue;
                }
                _ => {
                    cfg.problems.push(FleetSharedConfigProblem::UnknownKey(key));
                    continue;
                }
            };
            if !applied {
                cfg.problems
                    .push(FleetSharedConfigProblem::InvalidValue(key));
            }
        }
        Ok(cfg)
    }
}

/// Store `value` in `slot` if it is an integer above zero; otherwise leave the
/// default in place. Zero is refused because no key means "unlimited" or
/// "off": a zero quota or interval is a typo, not a setting.
fn positive(value: &serde_json::Value, slot: &mut u64) -> bool {
    match value.as_u64() {
        Some(n) if n > 0 => {
            *slot = n;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn parse(block: &str) -> FleetSharedAreaConfig {
        let cfg: Config = serde_yaml_ng::from_str(block).expect("config must still parse");
        cfg.fleet.shared_area
    }

    #[test]
    fn defaults_when_absent() {
        let c = parse("{}");
        assert_eq!(c.retention_days, 7);
        assert_eq!(c.max_object_kb, 128);
        assert_eq!(c.max_member_objects, 256);
        assert_eq!(c.max_member_mb, 32);
        assert_eq!(c.max_fleet_mb, 256);
        assert_eq!(c.max_total_mb, 1024);
        assert_eq!(c.on_expiry, FleetSharedOnExpiry::Delete);
        assert_eq!(c.request_timeout_secs, 30);
        assert_eq!(c.poll_secs, 1);
        assert!(c.problems.is_empty());
        assert!(c.is_default());
        assert_eq!(parse("fleet:\n  autorun: true\n"), c);
    }

    #[test]
    fn partial_override_keeps_the_other_defaults() {
        let c = parse("fleet:\n  shared_area:\n    max_object_kb: 64\n    on_expiry: delete\n");
        assert_eq!(c.max_object_kb, 64);
        assert_eq!(c.max_object_bytes(), 64 * 1024);
        assert_eq!(c.retention_days, 7);
        assert!(c.problems.is_empty());
        assert!(!c.is_default());
    }

    #[test]
    fn v2_keys_are_reported_and_have_no_effect() {
        let c = parse(
            "fleet:\n  autorun: true\n  shared_area:\n    max_file_mb: 4096\n    staging_grace_hours: 1\n    max_object_kb: 64\n",
        );
        assert_eq!(
            c.problems,
            vec![
                FleetSharedConfigProblem::V2Key("max_file_mb".into()),
                FleetSharedConfigProblem::V2Key("staging_grace_hours".into()),
            ]
        );
        assert_eq!(
            c.max_object_kb, 64,
            "the valid key beside them still applies"
        );
    }

    #[test]
    fn a_bad_block_never_costs_the_rest_of_the_config() {
        let cfg: Config = serde_yaml_ng::from_str(
            "fleet:\n  autorun: true\n  shared_area:\n    max_file_mb: 1\n    poll_secs: nope\n",
        )
        .unwrap();
        assert!(cfg.fleet.autorun);
    }

    #[test]
    fn invalid_values_fall_back_to_the_default_never_to_unlimited() {
        for bad in ["0", "-1", "1.5", "\"64\"", "unlimited", "~", "[1]", "true"] {
            let c = parse(&format!(
                "fleet:\n  shared_area:\n    max_total_mb: {bad}\n    poll_secs: {bad}\n"
            ));
            assert_eq!(c.max_total_mb, 1024, "max_total_mb: {bad}");
            assert_eq!(c.poll_secs, 1, "poll_secs: {bad}");
            assert_eq!(
                c.problems,
                vec![
                    FleetSharedConfigProblem::InvalidValue("max_total_mb".into()),
                    FleetSharedConfigProblem::InvalidValue("poll_secs".into()),
                ],
                "input {bad}"
            );
        }
        let c = parse("fleet:\n  shared_area:\n    on_expiry: keep\n");
        assert_eq!(c.on_expiry, FleetSharedOnExpiry::Delete);
        assert_eq!(
            c.problems,
            vec![FleetSharedConfigProblem::InvalidValue("on_expiry".into())]
        );
    }

    #[test]
    fn unknown_key_and_non_mapping_block_are_reported() {
        let c = parse("fleet:\n  shared_area:\n    max_objekt_kb: 1\n");
        assert_eq!(
            c.problems,
            vec![FleetSharedConfigProblem::UnknownKey("max_objekt_kb".into())]
        );
        assert!(c.is_default());
        for block in ["~", "7", "[]", "off"] {
            let c = parse(&format!("fleet:\n  shared_area: {block}\n"));
            assert_eq!(c.problems, vec![FleetSharedConfigProblem::NotAMapping]);
            assert!(c.is_default(), "block {block}");
        }
    }

    #[test]
    fn timeout_rounds_up_to_whole_polls() {
        let with = |timeout, poll| FleetSharedAreaConfig {
            request_timeout_secs: timeout,
            poll_secs: poll,
            ..Default::default()
        };
        assert_eq!(with(30, 1).effective_timeout_secs(), 30);
        assert_eq!(with(30, 7).effective_timeout_secs(), 35);
        assert_eq!(with(30, 10).effective_timeout_secs(), 30);
        assert_eq!(with(5, 60).effective_timeout_secs(), 60);
        // Rounding up past u64::MAX saturates: never below what was asked.
        assert_eq!(with(u64::MAX, 2).effective_timeout_secs(), u64::MAX);
    }

    #[test]
    fn byte_limits_saturate_instead_of_wrapping() {
        let c = FleetSharedAreaConfig {
            max_object_kb: u64::MAX,
            max_total_mb: u64::MAX,
            ..Default::default()
        };
        assert_eq!(c.max_object_bytes(), u64::MAX);
        assert_eq!(c.max_total_bytes(), u64::MAX);
    }

    #[test]
    fn a_default_block_is_not_written_and_a_changed_one_round_trips() {
        let mut cfg = Config::default();
        let yaml = serde_yaml_ng::to_string(&cfg).unwrap();
        assert!(!yaml.contains("shared_area"), "{yaml}");

        cfg.fleet.shared_area.max_member_mb = 8;
        let yaml = serde_yaml_ng::to_string(&cfg).unwrap();
        let back: Config = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(back.fleet.shared_area.max_member_mb, 8);
        assert!(back.fleet.shared_area.problems.is_empty());
    }
}
