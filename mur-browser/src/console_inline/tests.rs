use super::*;
use serde_json::json;

fn response(text: &str) -> Value {
    json!({"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":text}]}})
}

fn text_of(v: &Value) -> &str {
    v["result"]["content"][0]["text"].as_str().unwrap()
}

#[test]
fn range_link_is_replaced_by_the_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("console-x.log");
    std::fs::write(&log, "[  1ms] a\n[  2ms] b\n[  3ms] c\n").unwrap();
    let mut v = response(&format!(
        "### Page\n- URL: x\n### Events\n{CONSOLE_LINK_PREFIX}{}#L2-L3",
        log.display()
    ));
    inline_console_links(&mut v, tmp.path());
    let t = text_of(&v);
    assert!(t.contains("[  2ms] b") && t.contains("[  3ms] c"), "{t}");
    assert!(!t.contains("[  1ms] a"), "{t}");
    assert!(!t.contains("console-x.log"), "the link must be gone: {t}");
    assert!(t.starts_with("### Page\n- URL: x\n### Events\n"), "{t}");
}

#[test]
fn single_line_link_is_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("console-y.log");
    std::fs::write(&log, "only\n").unwrap();
    let mut v = response(&format!("{CONSOLE_LINK_PREFIX}{}#L1", log.display()));
    inline_console_links(&mut v, tmp.path());
    assert_eq!(text_of(&v), "- New console entries:\n  only");
}

#[test]
fn file_outside_root_is_not_read() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let log = other.path().join("secret.log");
    std::fs::write(&log, "TOP SECRET\n").unwrap();
    let mut v = response(&format!("{CONSOLE_LINK_PREFIX}{}#L1", log.display()));
    inline_console_links(&mut v, root.path());
    let t = text_of(&v);
    assert!(!t.contains("TOP SECRET"), "{t}");
    assert!(!t.contains("secret.log"), "{t}");
    assert!(t.contains("browser_console_messages"), "{t}");
}

#[test]
fn dotdot_escape_is_not_read() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("out");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(parent.path().join("x.log"), "ESCAPED\n").unwrap();
    let link = format!("{}/../x.log#L1", root.display());
    let mut v = response(&format!("{CONSOLE_LINK_PREFIX}{link}"));
    inline_console_links(&mut v, &root);
    assert!(!text_of(&v).contains("ESCAPED"));
}

#[test]
fn long_logs_are_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("console-z.log");
    let n = MAX_INLINE_LINES + 7;
    let body: String = (1..=n).map(|i| format!("line{i}\n")).collect();
    std::fs::write(&log, body).unwrap();
    let mut v = response(&format!("{CONSOLE_LINK_PREFIX}{}#L1-L{n}", log.display()));
    inline_console_links(&mut v, tmp.path());
    let t = text_of(&v);
    assert!(t.contains(&format!("line{MAX_INLINE_LINES}\n")), "{t}");
    assert!(!t.contains(&format!("line{}", MAX_INLINE_LINES + 1)), "{t}");
    assert!(t.contains("7 more"), "{t}");
}

#[test]
fn responses_without_links_are_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let original = json!({"jsonrpc":"2.0","id":1,"result":{"content":[
        {"type":"text","text":"### Page\n- URL: x"},
        {"type":"image","data":"AAAA","mimeType":"image/png"}
    ]}});
    let mut v = original.clone();
    inline_console_links(&mut v, tmp.path());
    assert_eq!(v, original);
    let mut err = json!({"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"x"}});
    inline_console_links(&mut err, tmp.path());
    assert_eq!(err["error"]["message"], "x");
}

#[test]
fn scan_keeps_only_the_displayed_lines_but_counts_all() {
    let n = MAX_INLINE_LINES * 40;
    let body: String = (1..=n).map(|i| format!("line{i}\n")).collect();
    let r = scan_lines(std::io::Cursor::new(body), 1, n).unwrap();
    assert_eq!(r.total, n);
    assert_eq!(r.shown.len(), MAX_INLINE_LINES);
    assert_eq!(r.shown[0], "line1");
}

#[test]
fn scan_bounds_one_giant_line_and_still_marks_truncation() {
    let giant = "é".repeat(MAX_LINE_CHARS * 100);
    let r = scan_lines(std::io::Cursor::new(format!("{giant}\nnext\n")), 1, 2).unwrap();
    assert_eq!(r.total, 2);
    assert!(
        r.shown[0].len() <= MAX_LINE_CHARS * 4 + 4,
        "kept {}",
        r.shown[0].len()
    );
    assert!(truncate(&r.shown[0], MAX_LINE_CHARS).ends_with('…'));
    assert_eq!(r.shown[1], "next");
}

#[test]
fn scan_honours_the_range_crlf_and_a_missing_final_newline() {
    let r = scan_lines(std::io::Cursor::new("a\r\nb\r\nc\r\nd"), 2, 9).unwrap();
    assert_eq!(r.shown, vec!["b", "c", "d"]);
    assert_eq!(r.total, 3);
}
