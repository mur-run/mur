use super::*;
use chrono::{Duration, TimeZone, Utc};
use std::path::{Path, PathBuf};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap()
}

fn req(host: &str, at: DateTime<Utc>) -> SshRequest {
    let t = RequestHost::parse(host, None).unwrap();
    SshRequest::new(&t, Some("deploy"), "inspect the project", at).unwrap()
}

fn dir(home: &Path) -> PathBuf {
    requests_dir(home)
}

// ---- validators ----

#[test]
fn hosts_are_dns_names_or_addresses_and_nothing_else() {
    for ok in [
        "people.example.edu",
        "Host-1.Example.COM",
        "my_alias",
        "10.0.0.7",
        "[::1]",
        "::1",
        "2001:db8::2",
    ] {
        assert!(RequestHost::parse(ok, None).is_ok(), "{ok}");
    }
    for bad in [
        "",
        "-oProxyCommand=x",
        "a b",
        "a,b",
        "a\nb",
        "a/b",
        "..",
        "a..b",
        ".a",
        "a.",
        "../../etc",
        "-a.example",
        "a-.example",
        "999.1.1.1",
        "fe80::1%en0",
        "[not-v6]",
        "héllo.example",
    ] {
        assert!(RequestHost::parse(bad, None).is_err(), "{bad:?}");
    }
    assert!(RequestHost::parse(&"a".repeat(254), None).is_err());
    assert!(RequestHost::parse(&format!("{}.x", "a".repeat(64)), None).is_err());
}

#[test]
fn canonical_form_lowercases_brackets_v6_and_makes_the_port_explicit() {
    let a = RequestHost::parse("Box.Example.com", None).unwrap();
    assert_eq!(a.canonical(), "box.example.com:22");
    assert_eq!(a.known_hosts_name(), "box.example.com");
    let b = RequestHost::parse("2001:DB8::2", Some(2222)).unwrap();
    assert_eq!(b.host(), "[2001:db8::2]");
    assert_eq!(b.connect_host(), "2001:db8::2");
    assert_eq!(b.canonical(), "[2001:db8::2]:2222");
    assert_eq!(b.known_hosts_name(), "[2001:db8::2]:2222");
    assert_eq!(RequestHost::parse("[2001:db8::2]", Some(2222)).unwrap(), b);
    assert!(matches!(
        RequestHost::parse("a", Some(0)),
        Err(RequestError::Port(0))
    ));
}

#[test]
fn the_id_is_the_sha256_of_the_canonical_host_port() {
    let a = RequestHost::parse("BOX.example.com", None).unwrap();
    let b = RequestHost::parse("box.example.com", Some(22)).unwrap();
    assert_eq!(a.id(), b.id());
    assert_eq!(a.id().len(), 64);
    assert!(
        a.id()
            .bytes()
            .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
    );
    assert_ne!(
        a.id(),
        RequestHost::parse("box.example.com", Some(2222))
            .unwrap()
            .id()
    );
}

#[test]
fn users_are_plain_login_names() {
    for ok in ["deploy", "_svc", "a.b-c_d", "u1"] {
        assert!(validate_user(ok).is_ok(), "{ok}");
    }
    for bad in [
        "",
        "-oX",
        "1abc",
        "a b",
        "a;b",
        "a@b",
        "root\n",
        &"a".repeat(33),
    ] {
        assert!(validate_user(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn reasons_lose_control_characters_and_are_capped() {
    assert_eq!(
        sanitize_reason("  check\tthe\n\nlogs\u{7}  "),
        "check the logs"
    );
    let long = "x".repeat(MAX_REASON_CHARS + 50);
    assert_eq!(sanitize_reason(&long).chars().count(), MAX_REASON_CHARS);
    let t = RequestHost::parse("a.example", None).unwrap();
    assert!(matches!(
        SshRequest::new(&t, None, " \u{1b} ", now()),
        Err(RequestError::EmptyReason)
    ));
}

// ---- store ----

#[test]
fn a_written_request_reads_back_and_a_repeat_replaces_it() {
    let home = tempfile::tempdir().unwrap();
    let w = write(home.path(), &req("box.example.com", now())).unwrap();
    assert!(!w.replaced);
    assert!(
        dir(home.path())
            .join(format!("{}.json", w.target.id()))
            .is_file()
    );
    let again = write(
        home.path(),
        &req("BOX.example.com", now() + Duration::minutes(1)),
    )
    .unwrap();
    assert!(again.replaced);
    let got = read_pending(home.path(), now() + Duration::minutes(2));
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].target.canonical(), "box.example.com:22");
    assert_eq!(got[0].user.as_deref(), Some("deploy"));
    assert_eq!(got[0].reason, "inspect the project");
}

#[test]
fn expired_unknown_schema_unknown_field_and_oversized_files_are_dropped() {
    let home = tempfile::tempdir().unwrap();
    let d = dir(home.path());
    // expired
    write(
        home.path(),
        &req("old.example", now() - Duration::seconds(EXPIRY_SECS + 1)),
    )
    .unwrap();
    // unknown schema, unknown field, oversized: written by hand, as an agent could
    let put = |host: &str, body: String| {
        let id = RequestHost::parse(host, None).unwrap().id();
        std::fs::write(d.join(format!("{id}.json")), body).unwrap();
    };
    put("s.example", r#"{"schema":2,"host":"s.example","port":22,"reason":"r","requested_at":"2026-11-11T12:00:00Z"}"#.into());
    put("f.example", r#"{"schema":1,"host":"f.example","port":22,"reason":"r","requested_at":"2026-11-11T12:00:00Z","grant":"all"}"#.into());
    put(
        "big.example",
        format!(
            r#"{{"schema":1,"host":"big.example","port":22,"reason":"{}","requested_at":"2026-11-11T12:00:00Z"}}"#,
            "r".repeat(MAX_FILE_BYTES as usize)
        ),
    );
    assert!(read_pending(home.path(), now()).is_empty());
}

#[test]
fn only_the_newest_eight_are_returned() {
    let home = tempfile::tempdir().unwrap();
    for i in 0..(MAX_PENDING + 3) {
        write(
            home.path(),
            &req(
                &format!("h{i}.example"),
                now() - Duration::minutes(i as i64),
            ),
        )
        .unwrap();
    }
    let got = read_pending(home.path(), now());
    assert_eq!(got.len(), MAX_PENDING);
    assert_eq!(got[0].target.host(), "h0.example");
    assert!(
        got.iter()
            .all(|p| p.target.host() != format!("h{}.example", MAX_PENDING + 2))
    );
}

#[test]
fn names_that_are_not_an_id_and_ids_that_do_not_match_are_skipped() {
    let home = tempfile::tempdir().unwrap();
    let d = dir(home.path());
    std::fs::create_dir_all(&d).unwrap();
    let body = serde_json::to_vec(&req("a.example", now())).unwrap();
    std::fs::write(d.join("notes.json"), &body).unwrap();
    std::fs::write(d.join(format!("{}.json", "A".repeat(64))), &body).unwrap();
    let other = RequestHost::parse("b.example", None).unwrap().id();
    std::fs::write(d.join(format!("{other}.json")), &body).unwrap();
    assert!(read_pending(home.path(), now()).is_empty());
}

#[test]
fn a_host_with_a_slash_or_dots_in_a_file_is_dropped() {
    let home = tempfile::tempdir().unwrap();
    let d = dir(home.path());
    std::fs::create_dir_all(&d).unwrap();
    for host in ["a/b", "..", "../../x"] {
        // The id of the bad host, so only host validation can reject it.
        let id = {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(format!("{host}:22").as_bytes()))
        };
        let body = format!(
            r#"{{"schema":1,"host":"{host}","port":22,"reason":"r","requested_at":"2026-11-11T12:00:00Z"}}"#
        );
        std::fs::write(d.join(format!("{id}.json")), body).unwrap();
    }
    assert!(read_pending(home.path(), now()).is_empty());
}

#[cfg(unix)]
#[test]
fn a_symlink_in_the_dir_is_neither_read_nor_deleted_through() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let w = write(home.path(), &req("a.example", now())).unwrap();
    let target_file = outside.path().join("victim.json");
    std::fs::rename(
        dir(home.path()).join(format!("{}.json", w.target.id())),
        &target_file,
    )
    .unwrap();
    std::os::unix::fs::symlink(
        &target_file,
        dir(home.path()).join(format!("{}.json", w.target.id())),
    )
    .unwrap();
    assert!(
        read_pending(home.path(), now()).is_empty(),
        "a link is not read"
    );

    // remove() of a value naming that id unlinks the link, never the target.
    let real = write(home.path(), &req("b.example", now())).unwrap();
    let p = read_pending(home.path(), now())
        .into_iter()
        .find(|p| p.target == real.target)
        .unwrap();
    std::fs::remove_file(dir(home.path()).join(format!("{}.json", real.target.id()))).unwrap();
    std::os::unix::fs::symlink(
        &target_file,
        dir(home.path()).join(format!("{}.json", real.target.id())),
    )
    .unwrap();
    remove(home.path(), &p).unwrap();
    assert!(target_file.is_file(), "the file outside was not deleted");
}

#[cfg(unix)]
#[test]
fn a_symlinked_requests_dir_is_not_followed() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let w = write(outside.path(), &req("a.example", now())).unwrap();
    std::os::unix::fs::symlink(requests_dir(outside.path()), dir(home.path())).unwrap();
    assert!(read_pending(home.path(), now()).is_empty());
    let p = read_pending(outside.path(), now()).pop().unwrap();
    assert!(remove(home.path(), &p).is_err());
    assert!(
        requests_dir(outside.path())
            .join(format!("{}.json", w.target.id()))
            .is_file()
    );
}

#[test]
fn remove_deletes_exactly_that_request_and_a_second_remove_is_fine() {
    let home = tempfile::tempdir().unwrap();
    write(home.path(), &req("a.example", now())).unwrap();
    write(home.path(), &req("b.example", now())).unwrap();
    let all = read_pending(home.path(), now());
    let a = all.iter().find(|p| p.target.host() == "a.example").unwrap();
    remove(home.path(), a).unwrap();
    remove(home.path(), a).unwrap();
    let left = read_pending(home.path(), now());
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].target.host(), "b.example");
}

#[test]
fn ssh_is_spawnable_under_any_and_when_listed() {
    use crate::agent::{ProcessesEntitlement, SpawnEntitlement, SpawnMode};
    let mut p = ProcessesEntitlement {
        spawn: SpawnEntitlement {
            mode: SpawnMode::None,
            allowed: vec![],
            allowed_dirs: vec![],
        },
    };
    for (mode, listed, want) in [
        (SpawnMode::Any, false, true),
        (SpawnMode::None, true, false),
        (SpawnMode::Allowlist, false, false),
        (SpawnMode::Allowlist, true, true),
        (SpawnMode::Strict, true, true),
    ] {
        p.spawn.mode = mode;
        p.spawn.allowed = if listed {
            vec![SSH_BINARY.into()]
        } else {
            vec![]
        };
        assert_eq!(ssh_spawnable(&p), want, "{mode:?} listed={listed}");
    }
}
