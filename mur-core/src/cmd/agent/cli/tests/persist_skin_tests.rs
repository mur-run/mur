//! `persist_skin_tests`, moved out of `cli/mod.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: dedented one level, nothing else.

use super::super::persist_skin;

/// `persist_skin` must go through the shared `save_config_at` writer
/// instead of hand-rolling its own serialise/write/rename — otherwise it
/// inherits the bug that writer was just fixed for: a typed `Config`
/// round-trip silently drops every top-level block it has no field for
/// (e.g. a hand-written `research_gateway` block).
#[test]
fn persist_skin_preserves_blocks_the_typed_config_does_not_know() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    std::fs::write(
        home.join("config.yaml"),
        "research_gateway:\n  brave_api_key_ref: keychain:mur/brave\n",
    )
    .unwrap();

    persist_skin(home, "dark").unwrap();

    let back = std::fs::read_to_string(home.join("config.yaml")).unwrap();
    assert!(back.contains("skin: dark"), "skin not written:\n{back}");
    assert!(back.contains("research_gateway"), "block dropped:\n{back}");
    assert!(
        back.contains("keychain:mur/brave"),
        "value dropped:\n{back}"
    );
}
