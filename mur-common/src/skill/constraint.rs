use semver::{Version, VersionReq};

#[derive(Debug, Clone)]
pub struct Constraint(pub VersionReq);

#[derive(Debug, thiserror::Error)]
pub enum ConstraintError {
    #[error("invalid version requirement '{0}': {1}")]
    Parse(String, String),
}

impl Constraint {
    /// Parse "requires:[].version".
    /// Accepts: `>=1.0.0`, `^1.2.3`, `~1.2.3`, exact `1.2.3`, `*`.
    /// Empty string and `*` both mean "any".
    pub fn parse(s: &str) -> Result<Self, ConstraintError> {
        let trimmed = s.trim();
        if trimmed.is_empty() || trimmed == "*" {
            return Ok(Self(VersionReq::STAR));
        }
        VersionReq::parse(trimmed)
            .map(Self)
            .map_err(|e| ConstraintError::Parse(trimmed.into(), e.to_string()))
    }

    pub fn matches(&self, v: &Version) -> bool {
        self.0.matches(v)
    }

    pub fn any() -> Self {
        Self(VersionReq::STAR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_matches_anything() {
        let c = Constraint::parse("*").unwrap();
        assert!(c.matches(&Version::parse("0.0.1").unwrap()));
        assert!(c.matches(&Version::parse("99.99.99").unwrap()));
    }

    #[test]
    fn ge_pins_minimum() {
        let c = Constraint::parse(">=1.0.0").unwrap();
        assert!(!c.matches(&Version::parse("0.9.9").unwrap()));
        assert!(c.matches(&Version::parse("1.0.0").unwrap()));
        assert!(c.matches(&Version::parse("2.0.0").unwrap()));
    }

    #[test]
    fn caret_locks_major() {
        let c = Constraint::parse("^1.2.3").unwrap();
        assert!(c.matches(&Version::parse("1.9.0").unwrap()));
        assert!(!c.matches(&Version::parse("2.0.0").unwrap()));
    }

    #[test]
    fn tilde_locks_minor() {
        let c = Constraint::parse("~1.2.3").unwrap();
        assert!(c.matches(&Version::parse("1.2.9").unwrap()));
        assert!(!c.matches(&Version::parse("1.3.0").unwrap()));
    }

    #[test]
    fn exact_pins_exact() {
        let c = Constraint::parse("=1.2.3").unwrap();
        assert!(c.matches(&Version::parse("1.2.3").unwrap()));
        assert!(!c.matches(&Version::parse("1.2.4").unwrap()));
    }

    #[test]
    fn empty_means_any() {
        let c = Constraint::parse("").unwrap();
        assert!(c.matches(&Version::parse("0.0.1").unwrap()));
    }

    #[test]
    fn garbage_fails() {
        assert!(Constraint::parse("not-a-version").is_err());
    }
}
