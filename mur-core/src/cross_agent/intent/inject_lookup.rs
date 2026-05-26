//! Read-side intent lookup used by the M6b inject path (M7c §3.6).
//!
//! Returns the canonical form for a given alias string, or the original
//! input when no mapping exists. Failures (missing file, parse error)
//! are silently treated as "no mapping" — the inject path never blocks
//! on a missing canonical file.

use std::collections::HashMap;
use std::path::Path;

use crate::cross_agent::intent::canonical::{IntentCanonical, normalise, read_canonical_yaml};

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct IntentLookup {
    by_alias: HashMap<String, String>,
    by_norm: HashMap<String, String>,
}

#[allow(dead_code)]
impl IntentLookup {
    pub fn empty() -> Self {
        Self {
            by_alias: HashMap::new(),
            by_norm: HashMap::new(),
        }
    }

    pub fn load(home: &Path) -> Self {
        match read_canonical_yaml(home) {
            Ok(Some(ic)) => Self::from(ic),
            _ => Self::empty(),
        }
    }

    pub fn from(ic: IntentCanonical) -> Self {
        let mut by_alias = HashMap::new();
        let mut by_norm = HashMap::new();
        for entry in &ic.canonical {
            for alias in &entry.aliases {
                by_alias.insert(alias.clone(), entry.canonical.clone());
            }
            by_norm.insert(normalise(&entry.canonical), entry.canonical.clone());
        }
        Self { by_alias, by_norm }
    }

    pub fn resolve_intent(&self, intent: &str) -> String {
        if let Some(c) = self.by_alias.get(intent) {
            return c.clone();
        }
        if let Some(c) = self.by_norm.get(&normalise(intent)) {
            return c.clone();
        }
        intent.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cross_agent::intent::canonical::CanonicalEntry;

    fn lookup_with(aliases: Vec<(&str, Vec<&str>)>) -> IntentLookup {
        let ic = IntentCanonical {
            version: 1,
            generated_at: chrono::Utc::now(),
            generated_by: "test".into(),
            canonical: aliases
                .into_iter()
                .map(|(c, a)| {
                    let count = a.len();
                    CanonicalEntry {
                        canonical: c.into(),
                        aliases: a.into_iter().map(String::from).collect(),
                        count,
                    }
                })
                .collect(),
        };
        IntentLookup::from(ic)
    }

    #[test]
    fn alias_resolves_to_canonical() {
        let l = lookup_with(vec![(
            "web_search",
            vec!["web_search", "search_web", "Web Search"],
        )]);
        assert_eq!(l.resolve_intent("search_web"), "web_search");
        assert_eq!(l.resolve_intent("Web Search"), "web_search");
        assert_eq!(l.resolve_intent("web_search"), "web_search");
    }

    #[test]
    fn unknown_returns_input() {
        let l = IntentLookup::empty();
        assert_eq!(l.resolve_intent("something_new"), "something_new");
    }
}
