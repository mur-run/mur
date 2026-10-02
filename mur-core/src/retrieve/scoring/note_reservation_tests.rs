use super::*;

struct Item {
    name: String,
    note: bool,
    importance: f64,
}
impl Retrievable for Item {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "topic thing"
    }
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed("topic body content")
    }
    fn tag_terms(&self) -> Vec<&str> {
        vec![]
    }
    fn importance(&self) -> f64 {
        self.importance
    }
    fn effectiveness(&self) -> f64 {
        1.0
    }
    fn tier(&self) -> Tier {
        Tier::Project
    }
    fn created_at(&self) -> chrono::DateTime<chrono::Utc> {
        Utc::now()
    }
    fn last_activity(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        Some(Utc::now())
    }
    fn decay_half_life_days(&self) -> f64 {
        90.0
    }
    fn is_active(&self) -> bool {
        true
    }
    fn is_note(&self) -> bool {
        self.note
    }
}

/// Six max-importance skills + one zero-importance note: the note always
/// ranks last, so without the reservation it never survives the 5-item cap.
fn skills_and_note() -> Vec<Item> {
    let mut v: Vec<Item> = (0..6)
        .map(|i| Item {
            name: format!("skill-{i}"),
            note: false,
            importance: 1.0,
        })
        .collect();
    v.push(Item {
        name: "the-note".into(),
        note: true,
        importance: 0.0,
    });
    v
}

fn cfg(reserved: usize) -> RetrievalConfig {
    RetrievalConfig {
        min_score: 0.0,
        reserved_note_slots: reserved,
        ..Default::default()
    }
}

#[test]
fn reserved_slot_swaps_in_the_best_note() {
    let r = score_and_rank_generic_with_config("topic", skills_and_note(), &cfg(1));
    assert_eq!(r.len(), 5, "item cap must be unchanged by the swap");
    assert!(
        r.last().unwrap().item.note,
        "the note takes the reserved tail slot"
    );
    assert_eq!(
        r.iter().filter(|s| s.item.note).count(),
        1,
        "exactly the reserved number of notes"
    );
}

#[test]
fn reservation_disabled_keeps_old_behavior() {
    let r = score_and_rank_generic_with_config("topic", skills_and_note(), &cfg(0));
    assert_eq!(r.len(), 5);
    assert!(
        !r.iter().any(|s| s.item.note),
        "reserved_note_slots=0 must opt out entirely"
    );
}

#[test]
fn no_swap_when_a_note_already_made_the_cut() {
    // The note outranks every skill: it earns its seat, nothing is swapped.
    let mut items = skills_and_note();
    items.last_mut().unwrap().importance = 1.0;
    for s in items.iter_mut().take(3) {
        s.importance = 0.0;
    }
    let r = score_and_rank_generic_with_config("topic", items, &cfg(1));
    assert_eq!(r.len(), 5);
    assert_eq!(r.iter().filter(|s| s.item.note).count(), 1);
    assert!(
        !r.last().unwrap().item.note || r.iter().take(4).all(|s| !s.item.note),
        "an organically-placed note is not duplicated by the reservation"
    );
}

#[test]
fn under_cap_result_is_untouched() {
    // Three items only: nothing overflows, reservation is a no-op.
    let items: Vec<Item> = vec![
        Item {
            name: "a".into(),
            note: false,
            importance: 1.0,
        },
        Item {
            name: "b".into(),
            note: true,
            importance: 0.5,
        },
        Item {
            name: "c".into(),
            note: false,
            importance: 0.9,
        },
    ];
    let r = score_and_rank_generic_with_config("topic", items, &cfg(1));
    assert_eq!(r.len(), 3, "all items fit; no swap path runs");
}
