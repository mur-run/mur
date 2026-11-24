//! #2538: every id handle the CLI prints names exactly the channel it was
//! printed for when typed back at `/channels`, including channels minted in
//! the same UUIDv7 time window, which share their first eight characters.

use super::{CHANNEL_LOOKUP_LIMIT, ChannelRef, RECENT_LIMIT, SlashCmd, parse_slash, resolve};
use crate::cmd::agent::cli::persist::{self, Session};
use tempfile::TempDir;

const AGENT: &str = "qa";
/// More than the listing shows, so the lookup window is exercised too.
const CHANNELS: usize = RECENT_LIMIT + 5;
/// A UUIDv7's first eight hex characters are the top 32 bits of its 48-bit
/// millisecond timestamp, so they change every 2^16 ms (~65.5 s).
const WINDOW_BITS: u32 = 16;
/// Start of one such window. Its prefix `019abcde` carries hex letters on
/// purpose: an all-digit handle is read back as an ordinal, not an id.
const WINDOW_START_MS: u64 = 0x019a_bcde << WINDOW_BITS;

/// The `n`th channel id inside one time window, so the ids share their first
/// eight characters however slowly the test runs (#2559). Minting them from
/// the wall clock failed whenever the loop crossed a window boundary.
fn id_in_window(n: u8) -> String {
    let ms = WINDOW_START_MS + u64::from(n);
    assert_eq!(ms >> WINDOW_BITS, WINDOW_START_MS >> WINDOW_BITS);
    uuid::Builder::from_unix_timestamp_millis(ms, &[n; 10])
        .into_uuid()
        .to_string()
}

/// A conversation with one user line, under [`id_in_window`]`(n)`.
fn session_in_window(home: &std::path::Path, n: u8, text: &str) -> Session {
    let mut s = Session::create_with_id(home, AGENT, &id_in_window(n)).unwrap();
    s.append("user", text, None, &[]).unwrap();
    s
}

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
        let n = u8::try_from(i).unwrap();
        session_in_window(tmp.path(), n, &format!("conversation {i}"));
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
    session_in_window(tmp.path(), 0, "one");
    let second = session_in_window(tmp.path(), 1, "two");

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
    let first = session_in_window(tmp.path(), 0, "one");
    let alone = first.current().unwrap().handle;
    assert_eq!(alone.len(), 8, "no neighbour yet: {alone}");

    session_in_window(tmp.path(), 1, "two");
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
