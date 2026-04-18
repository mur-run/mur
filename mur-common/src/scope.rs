use serde::{Deserialize, Serialize};

/// Scope of a pattern — the ownership/audience dimension, orthogonal to [`crate::Tier`]
/// (which manages temporal half-life).
///
/// - `Personal`: single-user, lives in `~/.mur/patterns/`
/// - `Team { team_id }`: shared within a team (requires paid Team tier)
/// - `Community { pack_id }`: public pattern, optionally part of a named pack
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Scope {
    #[default]
    Personal,
    Team { team_id: String },
    Community { pack_id: Option<String> },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_personal() {
        assert_eq!(Scope::default(), Scope::Personal);
    }

    #[test]
    fn yaml_roundtrip_personal() {
        let s = Scope::Personal;
        let y = serde_yaml::to_string(&s).unwrap();
        assert_eq!(y.trim(), "kind: personal");
        let back: Scope = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn yaml_roundtrip_team() {
        let s = Scope::Team { team_id: "ops".into() };
        let y = serde_yaml::to_string(&s).unwrap();
        let back: Scope = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn yaml_roundtrip_community_with_pack() {
        let s = Scope::Community { pack_id: Some("security-best-practices".into()) };
        let y = serde_yaml::to_string(&s).unwrap();
        let back: Scope = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn yaml_roundtrip_community_no_pack() {
        let s = Scope::Community { pack_id: None };
        let y = serde_yaml::to_string(&s).unwrap();
        let back: Scope = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back, s);
    }
}
