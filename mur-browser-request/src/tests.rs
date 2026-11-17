use super::*;
use chrono::{TimeZone, Utc};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap()
}

fn origin(raw: &str) -> String {
    Site::parse(raw).unwrap().origin()
}

#[test]
fn a_site_is_reduced_to_its_origin() {
    assert_eq!(origin("gitea.twdd.tw"), "https://gitea.twdd.tw");
    assert_eq!(
        origin("GITEA.twdd.tw/user/login?x=1"),
        "https://gitea.twdd.tw"
    );
    assert_eq!(origin("https://Example.com:443/a"), "https://example.com");
    assert_eq!(origin("http://example.com:80"), "http://example.com");
    assert_eq!(
        origin("http://gitea.twdd.tw:3000/explore"),
        "http://gitea.twdd.tw:3000"
    );
    assert_eq!(origin("gitea.twdd.tw:3000"), "https://gitea.twdd.tw:3000");
    assert_eq!(origin("http://10.0.0.5:8080"), "http://10.0.0.5:8080");
    assert_eq!(origin("http://[::1]:8080"), "http://[::1]:8080");
}

#[test]
fn host_and_port_are_what_the_card_matches_on() {
    let s = Site::parse("http://gitea.twdd.tw:3000/x").unwrap();
    assert_eq!(s.host(), "gitea.twdd.tw");
    assert_eq!(s.port(), Some(3000));
    assert_eq!(Site::parse("example.com").unwrap().port(), None);
}

#[test]
fn only_http_origins_without_userinfo_are_sites() {
    for bad in [
        "",
        "   ",
        "ftp://example.com",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "mailto:a@example.com",
        "https://user:pw@example.com",
        "https://user@example.com",
        "https://",
        "http://exa mple.com",
    ] {
        assert!(Site::parse(bad).is_err(), "{bad:?} must be refused");
    }
    let long = format!("https://example.com/{}", "a".repeat(MAX_SITE_LEN));
    assert!(Site::parse(&long).is_err());
}

#[test]
fn the_id_is_the_sha256_of_the_origin() {
    let s = Site::parse("https://example.com/path").unwrap();
    assert_eq!(s.id(), hex::encode(Sha256::digest(b"https://example.com")));
    assert_eq!(s.id(), Site::parse("EXAMPLE.com").unwrap().id());
    assert_ne!(s.id(), Site::parse("http://example.com").unwrap().id());
}

#[test]
fn reasons_lose_control_characters_and_are_capped() {
    assert_eq!(
        sanitize_reason("  log\tin \n\u{7}to  gitea "),
        "log in to gitea"
    );
    assert_eq!(
        sanitize_reason(&"x".repeat(MAX_REASON_CHARS + 9))
            .chars()
            .count(),
        MAX_REASON_CHARS
    );
    let site = Site::parse("example.com").unwrap();
    assert_eq!(
        BrowserRequest::new(&site, true, " \n\t", now()),
        Err(RequestError::EmptyReason)
    );
}

#[test]
fn a_request_carries_exactly_site_login_and_reason() {
    let site = Site::parse("http://gitea.twdd.tw:3000/x").unwrap();
    let r = BrowserRequest::new(&site, true, "review PR 12", now()).unwrap();
    let v = serde_json::to_value(&r).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["login", "reason", "requested_at", "schema", "site"]);
    assert_eq!(v["site"], "http://gitea.twdd.tw:3000");
    assert_eq!(v["login"], true);
}

// ---- files ----

use chrono::Duration;
use std::path::Path;

fn req(site: &str, at: DateTime<Utc>) -> BrowserRequest {
    BrowserRequest::new(&Site::parse(site).unwrap(), true, "log in", at).unwrap()
}

fn put(home: &Path, id: &str, body: &str) {
    let d = requests_dir(home);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(format!("{id}.json")), body).unwrap();
}

#[test]
fn a_written_request_reads_back_and_a_repeat_replaces_it() {
    let home = tempfile::tempdir().unwrap();
    let w = write(home.path(), &req("http://gitea.twdd.tw:3000/a", now())).unwrap();
    assert!(!w.replaced);
    assert_eq!(w.site.origin(), "http://gitea.twdd.tw:3000");
    let w = write(home.path(), &req("http://GITEA.twdd.tw:3000/b", now())).unwrap();
    assert!(w.replaced, "same origin, same file");
    let p = read_pending(home.path(), now());
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].site.origin(), "http://gitea.twdd.tw:3000");
    assert!(p[0].login);
    assert_eq!(p[0].reason, "log in");
}

#[test]
fn expired_unknown_schema_unknown_field_and_oversized_files_are_dropped() {
    let home = tempfile::tempdir().unwrap();
    write(
        home.path(),
        &req("old.example", now() - Duration::seconds(EXPIRY_SECS + 1)),
    )
    .unwrap();
    let id = |s: &str| Site::parse(s).unwrap().id();
    put(
        home.path(),
        &id("s.example"),
        r#"{"schema":2,"site":"https://s.example","login":true,"reason":"r","requested_at":"2026-11-15T12:00:00Z"}"#,
    );
    put(
        home.path(),
        &id("f.example"),
        r#"{"schema":1,"site":"https://f.example","login":true,"reason":"r","requested_at":"2026-11-15T12:00:00Z","profile":"work"}"#,
    );
    put(
        home.path(),
        &id("big.example"),
        &format!(
            r#"{{"schema":1,"site":"https://big.example","login":true,"reason":"{}","requested_at":"2026-11-15T12:00:00Z"}}"#,
            "r".repeat(MAX_FILE_BYTES as usize)
        ),
    );
    assert!(read_pending(home.path(), now()).is_empty());
}

#[test]
fn a_site_not_in_canonical_form_or_under_another_id_is_dropped() {
    let home = tempfile::tempdir().unwrap();
    let id = |s: &str| Site::parse(s).unwrap().id();
    // canonical would be "https://a.example"; the file carries a path
    put(
        home.path(),
        &id("a.example"),
        r#"{"schema":1,"site":"https://a.example/login","login":true,"reason":"r","requested_at":"2026-11-15T12:00:00Z"}"#,
    );
    // valid body filed under another origin's id
    put(
        home.path(),
        &id("c.example"),
        r#"{"schema":1,"site":"https://b.example","login":true,"reason":"r","requested_at":"2026-11-15T12:00:00Z"}"#,
    );
    // refused scheme, filed under the id its own string would hash to, so
    // the scheme is the only thing wrong with it
    put(
        home.path(),
        &hex::encode(Sha256::digest(b"file:///x")),
        r#"{"schema":1,"site":"file:///x","login":true,"reason":"r","requested_at":"2026-11-15T12:00:00Z"}"#,
    );
    put(
        home.path(),
        "not-an-id",
        r#"{"schema":1,"site":"https://d.example","login":true,"reason":"r","requested_at":"2026-11-15T12:00:00Z"}"#,
    );
    assert!(read_pending(home.path(), now()).is_empty());
}

#[test]
fn only_the_newest_eight_are_returned() {
    let home = tempfile::tempdir().unwrap();
    for i in 0..(MAX_PENDING + 3) {
        let at = now() - Duration::minutes(i as i64);
        write(home.path(), &req(&format!("h{i}.example"), at)).unwrap();
    }
    let p = read_pending(home.path(), now());
    assert_eq!(p.len(), MAX_PENDING);
    assert_eq!(p[0].site.host(), "h0.example");
}

#[cfg(unix)]
#[test]
fn a_symlinked_requests_dir_is_not_followed() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), &req("a.example", now())).unwrap();
    std::os::unix::fs::symlink(requests_dir(outside.path()), requests_dir(home.path())).unwrap();
    assert!(read_pending(home.path(), now()).is_empty());
    let p = read_pending(outside.path(), now()).pop().unwrap();
    assert!(remove(home.path(), &p).is_err());
    assert_eq!(read_pending(outside.path(), now()).len(), 1);
}

#[test]
fn remove_deletes_exactly_that_request_and_a_second_remove_is_fine() {
    let home = tempfile::tempdir().unwrap();
    write(home.path(), &req("a.example", now())).unwrap();
    write(home.path(), &req("b.example", now())).unwrap();
    let a = read_pending(home.path(), now())
        .into_iter()
        .find(|p| p.site.host() == "a.example")
        .unwrap();
    remove(home.path(), &a).unwrap();
    remove(home.path(), &a).unwrap();
    let left = read_pending(home.path(), now());
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].site.host(), "b.example");
}

#[test]
fn remove_keeps_a_newer_request_for_the_same_origin() {
    // The agent asked again while the card for the first ask was open: the
    // answer to the first must not delete the second unseen (review #2426).
    let home = tempfile::tempdir().unwrap();
    write(home.path(), &req("a.example", now())).unwrap();
    let shown = read_pending(home.path(), now()).pop().unwrap();
    let later = now() + chrono::Duration::seconds(90);
    let again = BrowserRequest::new(&shown.site, true, "log in again", later).unwrap();
    write(home.path(), &again).unwrap();
    remove(home.path(), &shown).unwrap();
    let left = read_pending(home.path(), later);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].reason, "log in again");
    assert_eq!(left[0].requested_at, later);
}

#[test]
fn a_request_dated_past_the_skew_allowance_is_dropped() {
    let home = tempfile::tempdir().unwrap();
    write(
        home.path(),
        &req(
            "future.example",
            now() + Duration::seconds(MAX_FUTURE_SKEW_SECS + 1),
        ),
    )
    .unwrap();
    write(
        home.path(),
        &req(
            "skewed.example",
            now() + Duration::seconds(MAX_FUTURE_SKEW_SECS),
        ),
    )
    .unwrap();
    let p = read_pending(home.path(), now());
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].site.origin(), "https://skewed.example");
}
