//! Track C3 / M-c3.6.3 — gate on the §5.5 acceptance footer being
//! marked shipped in the roadmap spec. Mirrors the §5.4 spec-tick
//! gate (verify_c2_spec_tick.rs) — same shape so a spec drift check
//! catches both Track C2 and Track C3 the same way.

const SPEC: &str = include_str!(
    "../../docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md"
);

/// Extract the body of §5.5 — from `### 5.5` up to the next `### `
/// header. Anchoring on the section header (not the changelog
/// reference at line 82) keeps the test stable against future
/// reshuffles of the changelog.
fn section_5_5_body() -> &'static str {
    let header = "### 5.5 C3";
    let start = SPEC.find(header).expect("§5.5 section header missing");
    let rest = &SPEC[start..];
    let next_header = rest[header.len()..]
        .find("\n### ")
        .map(|i| i + header.len())
        .unwrap_or(rest.len());
    &rest[..next_header]
}

#[test]
fn spec_section_5_5_marked_shipped() {
    let body = section_5_5_body();
    assert!(
        body.contains("Status: shipped") || body.contains("[shipped 2026-05-04]"),
        "§5.5 missing shipped marker. Body:\n{body}"
    );
}

#[test]
fn spec_section_5_5_dates_the_ship() {
    let body = section_5_5_body();
    assert!(
        body.contains("2026-05-04"),
        "§5.5 ship marker must carry an absolute date so future readers \
         can tell when this landed without git archaeology"
    );
}

#[test]
fn spec_section_5_5_acknowledges_deferred_production_wiring() {
    // Harness landing without lib.rs::setup wiring is the whole story
    // of this PR series. The spec footer must not pretend it's a full
    // ship — readers walking the spec → cookbook chain need to know
    // there's a stacked follow-up.
    let body = section_5_5_body();
    assert!(
        body.contains("follow-up") || body.contains("stacked"),
        "§5.5 footer must call out the deferred production wiring"
    );
}
