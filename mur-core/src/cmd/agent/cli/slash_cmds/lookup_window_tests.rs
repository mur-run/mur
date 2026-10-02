use super::{CHANNEL_LOOKUP_LIMIT, ChannelRef, RECENT_LIMIT, resolve};
use crate::cmd::agent::cli::persist::{self, Session};
use tempfile::TempDir;

#[test]
fn a_channel_pushed_off_the_listing_is_still_reachable_by_its_number() {
    let tmp = TempDir::new().unwrap();
    // The oldest channel is #1 and will be far off the end of a
    // newest-first listing of RECENT_LIMIT rows.
    for i in 0..(RECENT_LIMIT + 5) {
        let mut s = Session::create(tmp.path(), "qa").unwrap();
        s.append("user", &format!("conversation {i}"), None, &[])
            .unwrap();
    }
    let listed = persist::list_recent(tmp.path(), "qa", RECENT_LIMIT).unwrap();
    assert_eq!(listed.len(), RECENT_LIMIT, "sanity: the listing is capped");
    assert!(
        resolve(&listed, &ChannelRef::Ordinal(1)).is_err(),
        "sanity: #1 is off the listing, which is why the wider window exists"
    );

    let window = persist::list_recent(tmp.path(), "qa", CHANNEL_LOOKUP_LIMIT).unwrap();
    assert_eq!(
        resolve(&window, &ChannelRef::Ordinal(1))
            .expect("#1 must still resolve")
            .ordinal,
        1
    );
}
