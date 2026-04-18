use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Scope {
    Personal,
    Team { team_id: String },
    Community { pack_id: Option<String> },
}

impl Default for Scope {
    fn default() -> Self { Self::Personal }
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
}
