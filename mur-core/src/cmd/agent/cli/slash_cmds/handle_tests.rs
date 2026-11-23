//! #2538: every id handle the CLI prints names exactly the channel it was
//! printed for when typed back at `/channels`, including channels minted in
//! the same UUIDv7 time window, which share their first eight characters.

use super::{CHANNEL_LOOKUP_LIMIT, ChannelRef, RECENT_LIMIT, SlashCmd, parse_slash, resolve};
use crate::cmd::agent::cli::persist::{self, Session};
use tempfile::TempDir;

const AGENT: &str = "qa";
/// More than the listing shows, so the lookup window is exercised too.
const CHANNELS: usize = RECENT_LIMIT + 5;

/// What `/channels <handle>` parses to: the path a typed line takes.
fn typed_ref(handle: &str) -> ChannelRef {
    match parse_slash(&format!("/channels {handle}")) {
        Some(SlashCmd::Channels {
            target: Some(t), ..
        }) => t,
        other => panic!("/channels {handle} parsed as {other:?}"),
    }
}

#[test]
fn every_listed_handle_types_back_to_its_own_channel() {
    let tmp = TempDir::new().unwrap();
    crate::channel_writer::plant_identity_for(tmp.path(), AGENT);
    crate::channel_writer::plant_human_identity(tmp.path());
    for i in 0..CHANNELS {
        let mut s = Session::create(tmp.path(), AGENT).unwrap();
        s.append("user", &format!("conversation {i}"), None, &[])
            .unwrap();
    }
    let window = persist::list_recent(tmp.path(), AGENT, CHANNEL_LOOKUP_LIMIT).unwrap();
    assert_eq!(window.len(), CHANNELS);
    let first8 = |id: &str| id[..8].to_string();
    assert!(
        window
            .iter()
            .any(|s| first8(&s.id) == first8(&window[0].id) && s.id != window[0].id),
        "sanity: the channels share their first eight characters"
    );

    for row in &window {
        let typed = typed_ref(&row.handle);
        let hit = resolve(&window, &typed)
            .unwrap_or_else(|e| panic!("{} did not resolve: {e:?}", row.handle));
        assert_eq!(hit.id, row.id, "{} named another channel", row.handle);
    }
}

/// The footer chip shows the live channel's handle; it must be the same one
/// the listing prints, so copying either works.
#[test]
fn the_footer_handle_is_the_listing_handle() {
    let tmp = TempDir::new().unwrap();
    crate::channel_writer::plant_identity_for(tmp.path(), AGENT);
    crate::channel_writer::plant_human_identity(tmp.path());
    let mut first = Session::create(tmp.path(), AGENT).unwrap();
    first.append("user", "one", None, &[]).unwrap();
    let mut second = Session::create(tmp.path(), AGENT).unwrap();
    second.append("user", "two", None, &[]).unwrap();

    let meta = second.current().unwrap();
    let listed = persist::list_recent(tmp.path(), AGENT, RECENT_LIMIT).unwrap();
    let row = listed.iter().find(|s| s.id == meta.id).unwrap();
    assert_eq!(meta.handle, row.handle);
    assert!(
        meta.handle.len() > 8,
        "neighbours force a longer handle: {}",
        meta.handle
    );
}

/// A handle is unique when it is computed, not for ever: a conversation
/// minted beside this one lengthens it. The footer reads it again each turn,
/// so it never keeps showing a handle that has since become ambiguous —
/// which is what caching it in the session would do.
#[test]
fn the_footer_handle_lengthens_when_a_neighbour_appears() {
    let tmp = TempDir::new().unwrap();
    crate::channel_writer::plant_identity_for(tmp.path(), AGENT);
    crate::channel_writer::plant_human_identity(tmp.path());
    let mut first = Session::create(tmp.path(), AGENT).unwrap();
    first.append("user", "one", None, &[]).unwrap();
    let alone = first.current().unwrap().handle;
    assert_eq!(alone.len(), 8, "no neighbour yet: {alone}");

    let mut second = Session::create(tmp.path(), AGENT).unwrap();
    second.append("user", "two", None, &[]).unwrap();
    let crowded = first.current().unwrap().handle;
    assert!(crowded.len() > 8, "the neighbour shares {alone}: {crowded}");

    let window = persist::list_recent(tmp.path(), AGENT, CHANNEL_LOOKUP_LIMIT).unwrap();
    let own = first.current().unwrap().id;
    assert_eq!(resolve(&window, &typed_ref(&crowded)).unwrap().id, own);
    assert!(
        resolve(&window, &typed_ref(&alone)).is_err(),
        "the outgrown handle is refused as ambiguous, never resolved to a neighbour"
    );
}
