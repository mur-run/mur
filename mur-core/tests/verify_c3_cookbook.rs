//! Track C3 / M-c3.6.2 — gate on the cookbook covering every channel
//! and surfacing the v1 deferral notes. Tests live in mur-core
//! because that's where verify_*.rs siblings already are (b0,
//! companion-gui-bridge, c1-bridge, c2-telegram); the doc itself
//! sits in repo-root `docs/cookbook/` so the working dir resolves to
//! the workspace root.

const COOKBOOK: &str = include_str!("../../docs/cookbook/c3-send-from-any-app.md");

#[test]
fn cookbook_documents_all_four_channels() {
    for section in ["URL scheme", "Global hotkey", "Services menu", "Drag-to-dock"] {
        assert!(
            COOKBOOK.contains(section),
            "cookbook missing section: {section}"
        );
    }
}

#[test]
fn cookbook_explains_per_agent_slug_constraint() {
    // Each agent registers `muragent-<slug>://` — flag if the
    // cookbook drifts away from documenting why the slug is
    // per-agent (so a malicious page can't blast every running
    // agent at once).
    assert!(COOKBOOK.contains("muragent-<slug>"));
}

#[test]
fn cookbook_explains_v1_deferrals() {
    // Two specific things are out of scope for v1: a unified
    // `mur://` scheme + an `.appex` Share Extension. Both are
    // called out so a future contributor doesn't try to ship
    // either of them inside this PR series.
    assert!(COOKBOOK.contains("not in v1") || COOKBOOK.contains("Not in v1"));
    assert!(COOKBOOK.contains(".appex"));
}

#[test]
fn cookbook_documents_hotkey_collision_escape_hatch() {
    // `share.hotkey` in companion state.yaml is the override.
    // Two agents with the same first letter collide on the
    // default combo; the escape hatch must stay documented.
    assert!(COOKBOOK.contains("share.hotkey") || COOKBOOK.contains("share-hotkey"));
}

#[test]
fn cookbook_references_b0_untrusted_share_wrapping() {
    // The whole point of routing through SendIngestor is that
    // B0 wraps the body as <untrusted_share> with a one-turn
    // cooldown. If the cookbook stops mentioning that, a reader
    // could (incorrectly) assume shared content is trusted.
    assert!(COOKBOOK.contains("<untrusted_share>"));
}
