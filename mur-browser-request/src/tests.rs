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
