use mur_channel::ChannelService;
use mur_core::cmd::agent::cli::persist::Session;
use tempfile::TempDir;

// Simulates: CLI appends a turn; the "Hub side" (a second ChannelService on the
// same home) reads the very same event. This is the v1 payoff — one store, two
// surfaces.
#[test]
fn cli_write_is_visible_to_a_second_reader() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    // CLI side writes.
    let mut sess = Session::create(home, "qa").unwrap();
    sess.append("user", "shared message", None, &[]).unwrap();
    let cid = sess.channel_id().unwrap().to_string();

    // Hub side reads the same on-disk store.
    let hub = ChannelService::open(home).unwrap();
    let evs = hub.load_events(&cid).unwrap();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].payload["text"], "shared message");

    // And the index lists it for the agent.
    assert_eq!(
        hub.latest_for_agent("qa").unwrap().as_deref(),
        Some(cid.as_str())
    );
}

// Deleting the SQLite index and rebuilding from logs restores the listing.
#[test]
fn index_is_rebuildable_from_logs() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let mut sess = Session::create(home, "qa").unwrap();
    sess.append("user", "x", None, &[]).unwrap();

    // Close the session's open SQLite connection before deleting the index.
    // On Windows an open file cannot be removed (sharing violation); dropping
    // also clears the WAL side-files. Remove the whole index dir to fully nuke it.
    drop(sess);
    std::fs::remove_dir_all(home.join("index")).unwrap();

    // Rebuild from the on-disk logs and confirm the listing returns.
    let store = mur_channel::ChannelStore::new(home);
    let index = mur_channel::index::ChannelIndex::open(home).unwrap();
    let n = index.rebuild_from(&store).unwrap();
    assert_eq!(n, 1);
    assert_eq!(index.list(10).unwrap().len(), 1);
}
