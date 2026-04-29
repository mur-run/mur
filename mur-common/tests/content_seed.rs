use mur_common::companion::content_seed;

#[test]
fn all_seeds_parse_and_have_minimum_templates() {
    for (situation, locale, raw) in content_seed::all_seeds() {
        let parsed =
            content_seed::parse(raw).unwrap_or_else(|e| panic!("parse {situation:?} {locale}: {e}"));
        assert_eq!(parsed.situation, situation);
        assert_eq!(parsed.locale, locale);
        assert!(
            parsed.templates.len() >= 3,
            "{situation:?} {locale} has only {} templates (need ≥3)",
            parsed.templates.len()
        );
    }
}

#[test]
fn every_template_has_required_fields() {
    for (_, _, raw) in content_seed::all_seeds() {
        let parsed = content_seed::parse(raw).unwrap();
        for t in parsed.templates {
            assert!(!t.id.is_empty());
            assert!(t.weight > 0.0);
            assert!(t.cooldown_days >= 1);
            assert!(!t.source.is_empty());
            assert!(!t.reviewed_by.is_empty());
            assert!(!t.prompt_seed.trim().is_empty());
        }
    }
}

#[test]
fn template_ids_are_unique_within_a_file() {
    for (_, _, raw) in content_seed::all_seeds() {
        let parsed = content_seed::parse(raw).unwrap();
        let mut ids = std::collections::HashSet::new();
        for t in &parsed.templates {
            assert!(
                ids.insert(&t.id),
                "duplicate id {} in {:?}/{}",
                t.id,
                parsed.situation,
                parsed.locale
            );
        }
    }
}
