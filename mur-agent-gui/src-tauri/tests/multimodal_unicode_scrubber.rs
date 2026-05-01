use mur_agent_gui_lib::multimodal::unicode_scrubber::scrub;

#[test]
fn strips_tag_letters() {
    // U+E0049 U+E0067 U+E006E U+E006F U+E0072 U+E0065 = "Ignore" tag-encoded.
    let smuggled = "hello\u{E0049}\u{E0067}\u{E006E}\u{E006F}\u{E0072}\u{E0065} world";
    let (out, count) = scrub(smuggled);
    assert_eq!(out, "hello world");
    assert_eq!(count, 6);
}

#[test]
fn strips_bidi_overrides() {
    let s = "\u{202E}reverse\u{202C}";
    let (out, count) = scrub(s);
    assert_eq!(out, "reverse");
    assert_eq!(count, 2);
}

#[test]
fn strips_zwj() {
    let s = "a\u{200D}b";
    let (out, count) = scrub(s);
    assert_eq!(out, "ab");
    assert_eq!(count, 1);
}

#[test]
fn ascii_passthrough_zero_count() {
    let (out, count) = scrub("plain ascii text");
    assert_eq!(out, "plain ascii text");
    assert_eq!(count, 0);
}

#[test]
fn cjk_passthrough_zero_count() {
    let (out, count) = scrub("早安 你好");
    assert_eq!(out, "早安 你好");
    assert_eq!(count, 0);
}

#[test]
fn isolates_2066_to_2069_bidi_isolates() {
    // U+2066-U+2069 (LRI/RLI/FSI/PDI) all stripped.
    let s = "a\u{2066}b\u{2067}c\u{2068}d\u{2069}";
    let (out, count) = scrub(s);
    assert_eq!(out, "abcd");
    assert_eq!(count, 4);
}
