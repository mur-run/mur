//! Integration smoke test for `mur update`. Network-bound; gated on
//! MUR_UPDATE_NETWORK_TESTS=1 so CI doesn't fail when offline or rate-limited.

#[test]
fn fetch_latest_release_returns_a_tag() {
    if std::env::var("MUR_UPDATE_NETWORK_TESTS").ok().as_deref() != Some("1") {
        eprintln!("skipping: set MUR_UPDATE_NETWORK_TESTS=1 to run");
        return;
    }
    let r = mur_core::update::release::fetch_latest().expect("fetch_latest");
    assert!(r.tag_name.starts_with('v'));
    assert!(!r.assets.is_empty());
}
