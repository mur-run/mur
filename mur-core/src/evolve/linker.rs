//! A-Mem inspired pattern linking — Zettelkasten for patterns.
//!
//! When a new pattern is created, find related existing patterns
//! and create bidirectional links.

use mur_common::pattern::Pattern;

/// Discover links between a new pattern and existing patterns.
/// Returns pairs of (existing_pattern_name, link_type).
pub fn discover_links(new_pattern: &Pattern, existing: &[Pattern]) -> Vec<LinkSuggestion> {
    let mut suggestions = Vec::new();
    let new_text = format!(
        "{} {} {}",
        new_pattern.name,
        new_pattern.description,
        new_pattern.content.as_text()
    )
    .to_lowercase();

    let new_tags: std::collections::HashSet<&str> = new_pattern
        .tags
        .topics
        .iter()
        .chain(new_pattern.tags.languages.iter())
        .map(|s| s.as_str())
        .collect();

    for existing_p in existing {
        if existing_p.name == new_pattern.name {
            continue;
        }

        let existing_text = format!(
            "{} {} {}",
            existing_p.name,
            existing_p.description,
            existing_p.content.as_text()
        )
        .to_lowercase();

        let existing_tags: std::collections::HashSet<&str> = existing_p
            .tags
            .topics
            .iter()
            .chain(existing_p.tags.languages.iter())
            .map(|s| s.as_str())
            .collect();

        // Tag overlap score
        let tag_overlap = new_tags.intersection(&existing_tags).count();
        let tag_total = new_tags.len().max(existing_tags.len()).max(1);
        let tag_score = tag_overlap as f64 / tag_total as f64;

        // Keyword overlap (simple Jaccard on significant words)
        let new_words: std::collections::HashSet<&str> = new_text
            .split_whitespace()
            .filter(|w| w.len() > 3)
            .collect();
        let existing_words: std::collections::HashSet<&str> = existing_text
            .split_whitespace()
            .filter(|w| w.len() > 3)
            .collect();
        let word_overlap = new_words.intersection(&existing_words).count();
        let word_total = new_words.union(&existing_words).count().max(1);
        let word_score = word_overlap as f64 / word_total as f64;

        // Combined score
        let score = tag_score * 0.6 + word_score * 0.4;

        if score > 0.3 {
            // Check for supersedes relationship
            let link_type = if is_supersedes(new_pattern, existing_p) {
                LinkType::Supersedes
            } else {
                LinkType::Related
            };

            suggestions.push(LinkSuggestion {
                target_name: existing_p.name.clone(),
                link_type,
                score,
            });
        }
    }

    // Sort by score descending, limit to top 5
    suggestions.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    suggestions.truncate(5);
    suggestions
}

/// Check if new pattern supersedes (replaces) an existing one.
fn is_supersedes(new: &Pattern, existing: &Pattern) -> bool {
    // Same name prefix suggests replacement
    let new_base = new.name.split('-').take(2).collect::<Vec<_>>().join("-");
    let existing_base = existing.name.split('-').take(2).collect::<Vec<_>>().join("-");

    if new_base == existing_base && new.name != existing.name {
        return true;
    }

    // Check content for "instead of", "replaces", "deprecated"
    let new_content = new.content.as_text().to_lowercase();
    let existing_name_words: Vec<&str> = existing.name.split('-').collect();
    for word in &existing_name_words {
        if new_content.contains(&format!("instead of {}", word))
            || new_content.contains(&format!("replaces {}", word))
        {
            return true;
        }
    }

    false
}

/// Apply discovered links to patterns (mutates both sides).
pub fn apply_links(
    new_pattern: &mut Pattern,
    existing: &mut [Pattern],
    suggestions: &[LinkSuggestion],
) {
    for suggestion in suggestions {
        match suggestion.link_type {
            LinkType::Related => {
                if !new_pattern.links.related.contains(&suggestion.target_name) {
                    new_pattern.links.related.push(suggestion.target_name.clone());
                }
                // Bidirectional
                if let Some(target) = existing.iter_mut().find(|p| p.name == suggestion.target_name) {
                    if !target.links.related.contains(&new_pattern.name) {
                        target.links.related.push(new_pattern.name.clone());
                    }
                }
            }
            LinkType::Supersedes => {
                if !new_pattern.links.supersedes.contains(&suggestion.target_name) {
                    new_pattern.links.supersedes.push(suggestion.target_name.clone());
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct LinkSuggestion {
    pub target_name: String,
    pub link_type: LinkType,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LinkType {
    Related,
    Supersedes,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::pattern::*;

    fn make_pattern(name: &str, desc: &str, topics: Vec<&str>) -> Pattern {
        Pattern {
            schema: 2,
            name: name.into(),
            description: desc.into(),
            content: Content::Plain(desc.into()),
            tier: Tier::Session,
            importance: 0.5,
            confidence: 0.5,
            tags: Tags {
                topics: topics.into_iter().map(String::from).collect(),
                languages: vec![],
                extra: Default::default(),
            },
            applies: Applies::default(),
            evidence: Evidence::default(),
            links: Links::default(),
            lifecycle: Lifecycle::default(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_discover_related_by_tags() {
        let new = make_pattern("swift-testing-v2", "Swift Testing with macros", vec!["swift", "testing"]);
        let existing = vec![
            make_pattern("swift-testing-v1", "Old XCTest patterns", vec!["swift", "testing"]),
            make_pattern("rust-error-handling", "Anyhow usage", vec!["rust", "errors"]),
        ];

        let links = discover_links(&new, &existing);
        assert!(!links.is_empty());
        assert_eq!(links[0].target_name, "swift-testing-v1");
    }

    #[test]
    fn test_supersedes_detection() {
        let new = make_pattern("swift-testing-v2", "Use @Test instead of XCTest", vec!["swift"]);
        let old = make_pattern("swift-testing-v1", "Use XCTest", vec!["swift"]);

        let links = discover_links(&new, &[old]);
        assert!(!links.is_empty());
        assert_eq!(links[0].link_type, LinkType::Supersedes);
    }

    #[test]
    fn test_no_self_link() {
        let p = make_pattern("test", "test", vec!["a"]);
        let links = discover_links(&p, &[p.clone()]);
        assert!(links.is_empty());
    }

    #[test]
    fn test_apply_bidirectional() {
        let mut new = make_pattern("new-one", "new", vec!["swift"]);
        let mut existing = vec![make_pattern("old-one", "old", vec!["swift"])];
        let suggestions = vec![LinkSuggestion {
            target_name: "old-one".into(),
            link_type: LinkType::Related,
            score: 0.8,
        }];

        apply_links(&mut new, &mut existing, &suggestions);
        assert!(new.links.related.contains(&"old-one".to_string()));
        assert!(existing[0].links.related.contains(&"new-one".to_string()));
    }

    #[test]
    fn test_no_links_for_unrelated() {
        let new = make_pattern("swift-ui", "SwiftUI views", vec!["swift", "ui"]);
        let existing = vec![make_pattern("python-django", "Django ORM", vec!["python", "django"])];

        let links = discover_links(&new, &existing);
        assert!(links.is_empty());
    }
}
