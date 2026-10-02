use super::{ChannelRef, ResolveErr, resolve};
use crate::cmd::agent::cli::persist::SessionInfo;

fn si(id: &str, ordinal: u64) -> SessionInfo {
    SessionInfo {
        id: id.into(),
        preview: String::new(),
        turns: 1,
        ordinal,
    }
}

fn recent() -> Vec<SessionInfo> {
    // Newest-first, so list position and ordinal deliberately disagree.
    vec![
        si("01a0d420beef", 7),
        si("01a0d999cafe", 2),
        si("0bbb1111", 5),
    ]
}

#[test]
fn an_ordinal_matches_the_number_not_the_list_position() {
    let r = recent();
    assert_eq!(resolve(&r, &ChannelRef::Ordinal(2)).unwrap().id, r[1].id);
    assert_eq!(resolve(&r, &ChannelRef::Ordinal(7)).unwrap().id, r[0].id);
}

#[test]
fn an_unknown_ordinal_is_not_found() {
    assert!(matches!(
        resolve(&recent(), &ChannelRef::Ordinal(99)),
        Err(ResolveErr::NotFound)
    ));
}

#[test]
fn an_id_prefix_resolves_when_it_is_unique() {
    assert_eq!(
        resolve(&recent(), &ChannelRef::IdPrefix("01a0d420".into()))
            .unwrap()
            .ordinal,
        7
    );
}

/// Switching to the wrong conversation is silent and confusing, so a
/// prefix shared by two channels refuses rather than picking one.
#[test]
fn a_shared_id_prefix_is_ambiguous_and_names_the_candidates() {
    match resolve(&recent(), &ChannelRef::IdPrefix("01a0d".into())) {
        Err(ResolveErr::Ambiguous(ids)) => {
            assert_eq!(ids, vec!["01a0d420".to_string(), "01a0d999".to_string()]);
        }
        other => panic!("expected ambiguous, got {other:?}"),
    }
}

#[test]
fn a_too_short_id_prefix_is_refused_before_matching() {
    assert!(matches!(
        resolve(&recent(), &ChannelRef::IdPrefix("01".into())),
        Err(ResolveErr::TooShort)
    ));
}

/// Ordinals start at 1. `0` is the "not numbered yet" marker carried by a
/// row that predates the ordinals table and has not been backfilled — so
/// a bare `0` must never resolve, or `/channels 0` silently switches to
/// whichever unbackfilled channel happens to be listed first.
#[test]
fn ordinal_zero_never_resolves_even_when_a_row_is_unnumbered() {
    let r = vec![
        si("01a0d420beef", 0),
        si("01a0d999cafe", 0),
        si("0bbb1111", 5),
    ];
    assert!(matches!(
        resolve(&r, &ChannelRef::Ordinal(0)),
        Err(ResolveErr::NotFound)
    ));
}

/// The listing is where the user reads the number back, so an unnumbered
/// row must not print `0` there: it looks like a handle they can type.
#[test]
fn the_listing_blanks_the_number_for_an_unnumbered_channel() {
    assert!(super::channel_line(&si("01a0d420beef", 7)).starts_with("  7 · "));
    let unnumbered = super::channel_line(&si("01a0d420beef", 0));
    assert!(
        unnumbered.starts_with("  - · "),
        "unnumbered rows show a placeholder, not 0: {unnumbered}"
    );
}
/// A word that parsed as neither number nor id must say so. Falling back
/// to the listing would look like the command had worked.
#[test]
fn a_malformed_target_names_the_word_it_could_not_read() {
    let t = ChannelRef::Malformed("zzz".into());
    let err = resolve(&recent(), &t).unwrap_err();
    assert!(matches!(err, ResolveErr::NotFound));
    let msg = super::resolve_msg(&t, &err);
    assert!(msg.contains("zzz"), "must quote what the user typed: {msg}");
}
