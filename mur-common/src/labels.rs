//! Fleet labels — a central, many-to-many taxonomy over fleets.
//!
//! Labels live in one registry (`~/.mur/labels.yaml`), never in each
//! `fleet.yaml`: renaming a label must not rewrite N fleet files, and the
//! registry is also where label order (hence chip order) is kept.
//!
//! A fleet's **primary label is simply the first entry of its ordered
//! assignment list** — there is deliberately no separate `primary:` field, so
//! the two can never disagree.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A label id must be a filesystem-safe lowercase slug: it is used as a map key
/// in YAML and shown as a chip. Same character class as `valid_fleet_name`, so
/// a hand-edited `../evil` can never enter the registry.
pub fn valid_label_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub id: String,
    /// Human-facing name; falls back to `id` when empty.
    #[serde(default)]
    pub display: String,
    /// Optional chip tint, e.g. `#4a9eff`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

impl Label {
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        Label {
            display: id.clone(),
            id,
            color: None,
        }
    }

    pub fn display_or_id(&self) -> &str {
        if self.display.is_empty() {
            &self.id
        } else {
            &self.display
        }
    }
}

/// The whole taxonomy: an ordered list of labels plus fleet → label-ids.
///
/// `assignments` order is meaningful (index 0 is the primary label); the map
/// itself is a `BTreeMap` so the serialized file is stable across saves.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelRegistry {
    #[serde(default)]
    pub labels: Vec<Label>,
    #[serde(default)]
    pub assignments: BTreeMap<String, Vec<String>>,
}

impl LabelRegistry {
    pub fn contains(&self, id: &str) -> bool {
        self.labels.iter().any(|l| l.id == id)
    }

    pub fn get(&self, id: &str) -> Option<&Label> {
        self.labels.iter().find(|l| l.id == id)
    }

    /// Labels assigned to a fleet, primary first. Empty when unassigned.
    pub fn labels_of(&self, fleet: &str) -> &[String] {
        self.assignments.get(fleet).map(|v| &v[..]).unwrap_or(&[])
    }

    /// The group a fleet belongs to: its first label, or `None` for Ungrouped.
    pub fn primary_of(&self, fleet: &str) -> Option<&str> {
        self.labels_of(fleet).first().map(|s| s.as_str())
    }

    /// How many fleets carry a label (in any position).
    pub fn fleet_count(&self, id: &str) -> usize {
        self.assignments
            .values()
            .filter(|ids| ids.iter().any(|i| i == id))
            .count()
    }

    /// Self-heal a registry that may have been hand-edited: drop invalid or
    /// unknown label ids, de-duplicate while keeping first-wins order (so the
    /// primary survives), and drop fleets left with nothing.
    pub fn normalize(&mut self) {
        self.labels.retain(|l| valid_label_id(&l.id));
        let mut seen_labels = Vec::new();
        self.labels.retain(|l| {
            if seen_labels.contains(&l.id) {
                false
            } else {
                seen_labels.push(l.id.clone());
                true
            }
        });
        let known = seen_labels;
        for ids in self.assignments.values_mut() {
            let mut seen = Vec::new();
            ids.retain(|id| {
                if !known.contains(id) || seen.contains(id) {
                    false
                } else {
                    seen.push(id.clone());
                    true
                }
            });
        }
        self.assignments.retain(|_, ids| !ids.is_empty());
    }

    /// Replace a fleet's labels (order = priority; first is primary).
    pub fn set_labels(&mut self, fleet: &str, ids: Vec<String>) {
        if ids.is_empty() {
            self.assignments.remove(fleet);
        } else {
            self.assignments.insert(fleet.to_string(), ids);
        }
        self.normalize();
    }

    /// Remove a label everywhere: from the list and from every assignment.
    /// Fleets fall back to their next label, or become Ungrouped.
    pub fn delete_label(&mut self, id: &str) {
        self.labels.retain(|l| l.id != id);
        self.normalize();
    }

    /// Rename a label's display text (the id, being the key, is stable).
    pub fn rename_label(&mut self, id: &str, display: &str) -> bool {
        match self.labels.iter_mut().find(|l| l.id == id) {
            Some(l) => {
                l.display = display.to_string();
                true
            }
            None => false,
        }
    }

    /// Forget assignments for fleets that no longer exist on disk.
    pub fn prune(&mut self, existing_fleets: &[String]) {
        self.assignments
            .retain(|fleet, _| existing_fleets.iter().any(|f| f == fleet));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_id_refuses_traversal_and_junk() {
        assert!(valid_label_id("web"));
        assert!(valid_label_id("rust-2024_x"));
        assert!(!valid_label_id(""));
        assert!(!valid_label_id("../evil"));
        assert!(!valid_label_id("Web"));
        assert!(!valid_label_id("has space"));
        assert!(!valid_label_id(&"x".repeat(33)));
    }

    fn reg() -> LabelRegistry {
        let mut r = LabelRegistry {
            labels: vec![Label::new("web"), Label::new("rust")],
            assignments: BTreeMap::new(),
        };
        r.set_labels("develop-web", vec!["web".into(), "rust".into()]);
        r.set_labels("rust-solo", vec!["rust".into()]);
        r
    }

    #[test]
    fn primary_is_the_first_label_so_a_fleet_groups_once() {
        let r = reg();
        assert_eq!(r.primary_of("develop-web"), Some("web"));
        assert_eq!(r.primary_of("rust-solo"), Some("rust"));
        assert_eq!(r.primary_of("unknown-fleet"), None);
        assert_eq!(r.fleet_count("rust"), 2);
        assert_eq!(r.fleet_count("web"), 1);
    }

    #[test]
    fn normalize_drops_unknown_and_duplicate_ids() {
        let mut r = reg();
        r.assignments
            .insert("ghost".into(), vec!["nope".into(), "web".into(), "web".into()]);
        r.assignments.insert("empty".into(), vec!["nope".into()]);
        r.normalize();
        assert_eq!(r.labels_of("ghost"), ["web"]);
        assert!(!r.assignments.contains_key("empty"));
    }

    #[test]
    fn delete_label_scrubs_assignments_and_repoints_primary() {
        let mut r = reg();
        r.delete_label("web");
        assert!(!r.contains("web"));
        // develop-web falls back to its next label; it does not vanish.
        assert_eq!(r.primary_of("develop-web"), Some("rust"));
    }

    #[test]
    fn delete_last_label_makes_fleet_ungrouped() {
        let mut r = reg();
        r.delete_label("rust");
        assert_eq!(r.primary_of("rust-solo"), None);
    }

    #[test]
    fn prune_forgets_dead_fleets() {
        let mut r = reg();
        r.prune(&["develop-web".to_string()]);
        assert!(r.assignments.contains_key("develop-web"));
        assert!(!r.assignments.contains_key("rust-solo"));
    }

    #[test]
    fn rename_changes_display_not_id() {
        let mut r = reg();
        assert!(r.rename_label("web", "Web Stuff"));
        assert_eq!(r.get("web").unwrap().display_or_id(), "Web Stuff");
        assert!(!r.rename_label("missing", "x"));
    }
}
