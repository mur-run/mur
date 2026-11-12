use super::*;
use crate::state::{read_state, write_state};
use std::time::Duration;

#[derive(Default)]
struct MemoryStore(std::sync::Mutex<std::collections::HashMap<String, String>>);
impl StateKeyStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> Result<()> {
        self.0.lock().unwrap().insert(account.into(), value.into());
        Ok(())
    }
}

fn soon() -> Instant {
    Instant::now() + WRITEBACK_LOCK_WAIT
}

fn far() -> Instant {
    Instant::now() + crate::abandon::LOCK_WAIT
}

const OLD: &[u8] = br#"{"cookies":[],"origins":[{"origin":"https://chat.example.com","localStorage":[{"name":"userToken","value":"old"}]}]}"#;

fn setup(home: &Path, store: &MemoryStore) {
    std::fs::create_dir_all(paths::profile_dir(home, "ex")).unwrap();
    write_state(&paths::profile_state(home, "ex"), OLD, store).unwrap();
    let meta = auth::ProfileMeta {
        url: "https://chat.example.com/sign_in".into(),
        last_auth: DateTime::from_timestamp(1_600_000_000, 0).unwrap(),
        earliest_cookie_expires: DateTime::from_timestamp(1_600_001_800, 0),
        allow_domains: vec!["example.com".into()],
        last_refresh: None,
        writeback_abandoned: None,
    };
    auth::write_meta(&paths::profile_meta(home, "ex"), &meta).unwrap();
}

fn meta(home: &Path) -> auth::ProfileMeta {
    serde_yaml::from_str(&std::fs::read_to_string(paths::profile_meta(home, "ex")).unwrap())
        .unwrap()
}

#[test]
fn logged_in_export_replaces_the_profile_and_keeps_last_auth() {
    let home = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    setup(home.path(), &store);
    let dir = scratch_dir(home.path()).unwrap();
    let export = export_path(dir.path());
    let new = br#"{"cookies":[],"origins":[{"origin":"https://chat.example.com","localStorage":[{"name":"userToken","value":"new"}]}]}"#;
    std::fs::write(&export, new).unwrap();
    let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();

    let got = refresh_profile(home.path(), "ex", None, &export, now, &store, soon()).unwrap();

    assert_eq!(got, Outcome::Refreshed);
    assert_eq!(
        read_state(&paths::profile_state(home.path(), "ex"), &store).unwrap(),
        new
    );
    let m = meta(home.path());
    assert_eq!(
        m.last_auth,
        DateTime::from_timestamp(1_600_000_000, 0).unwrap()
    );
    assert_eq!(m.last_refresh, Some(now));
    // The stale 30-minute anti-bot expiry is gone: no session cookie.
    assert_eq!(m.earliest_cookie_expires, None);
    assert!(!export.exists(), "plaintext export must be removed");
}

#[test]
fn logged_out_or_missing_export_never_overwrites_the_profile() {
    let home = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    setup(home.path(), &store);
    let dir = scratch_dir(home.path()).unwrap();
    let export = export_path(dir.path());
    let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();

    let missing = refresh_profile(home.path(), "ex", None, &export, now, &store, soon()).unwrap();
    assert!(matches!(missing, Outcome::Skipped(_)));

    std::fs::write(
        &export,
        br#"{"cookies":[{"name":"__cf_bm","domain":".example.com"}],"origins":[]}"#,
    )
    .unwrap();
    let blank = refresh_profile(home.path(), "ex", None, &export, now, &store, soon()).unwrap();
    assert!(matches!(blank, Outcome::Skipped(_)));

    assert_eq!(
        read_state(&paths::profile_state(home.path(), "ex"), &store).unwrap(),
        OLD
    );
    assert_eq!(meta(home.path()).last_refresh, None);
    assert!(!export.exists());
}

#[test]
fn logout_that_keeps_preferences_leaves_the_encrypted_profile_unchanged() {
    let home = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    setup(home.path(), &store);
    let before = std::fs::read(paths::profile_state(home.path(), "ex")).unwrap();
    let dir = scratch_dir(home.path()).unwrap();
    let export = export_path(dir.path());
    std::fs::write(
        &export,
        br#"{"cookies":[],"origins":[{"origin":"https://chat.example.com","localStorage":[{"name":"theme","value":"dark"}]}]}"#,
    )
    .unwrap();
    let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();

    let got = refresh_profile(home.path(), "ex", None, &export, now, &store, soon()).unwrap();

    assert!(matches!(got, Outcome::Skipped(_)));
    assert_eq!(
        std::fs::read(paths::profile_state(home.path(), "ex")).unwrap(),
        before,
        "encrypted profile must be byte-for-byte unchanged"
    );
    assert_eq!(meta(home.path()).last_refresh, None);
    assert!(!export.exists());
}

#[test]
fn logout_that_clears_the_token_value_leaves_the_encrypted_profile_unchanged() {
    let home = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    setup(home.path(), &store);
    let before = std::fs::read(paths::profile_state(home.path(), "ex")).unwrap();
    let dir = scratch_dir(home.path()).unwrap();
    let export = export_path(dir.path());
    // `localStorage.setItem("userToken", "")`: the key stays, the token is gone.
    std::fs::write(
        &export,
        br#"{"cookies":[],"origins":[{"origin":"https://chat.example.com","localStorage":[{"name":"userToken","value":""},{"name":"theme","value":"dark"}]}]}"#,
    )
    .unwrap();
    let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();

    let got = refresh_profile(home.path(), "ex", None, &export, now, &store, soon()).unwrap();

    assert!(matches!(got, Outcome::Skipped(_)));
    assert_eq!(
        std::fs::read(paths::profile_state(home.path(), "ex")).unwrap(),
        before,
        "encrypted profile must be byte-for-byte unchanged"
    );
    assert_eq!(meta(home.path()).last_refresh, None);
    assert!(!export.exists());
}

#[test]
fn export_code_quotes_hostile_paths() {
    let code = export_code(Path::new("/private/a\"b\\c/storage-state.json")).unwrap();
    assert!(code.contains("storageState({ path: \"/private/a\\\"b\\\\c/storage-state.json\" })"));
}

/// Blocker 1 (#2360): a merged session whose export holds a cookie for a
/// host both profiles cover writes the rest back, drops that cookie from
/// both, and leaves each profile one notice naming the host, the other
/// profile and the recovery — even though its own write-back succeeded.
#[test]
fn an_overlap_drop_is_reported_on_each_profile_even_when_the_rest_is_saved() {
    let home = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    for (site, host) in [("a", "a.example.com"), ("b", "b.example.com")] {
        let dir = paths::profile_dir(home.path(), site);
        std::fs::create_dir_all(&dir).unwrap();
        let old = format!(
            r#"{{"cookies":[],"origins":[{{"origin":"https://{host}","localStorage":[{{"name":"userToken","value":"old"}}]}}]}}"#
        );
        write_state(
            &paths::profile_state(home.path(), site),
            old.as_bytes(),
            &store,
        )
        .unwrap();
        auth::write_meta(
            &paths::profile_meta(home.path(), site),
            &auth::ProfileMeta {
                url: format!("https://{host}/login"),
                last_auth: DateTime::from_timestamp(1_600_000_000, 0).unwrap(),
                earliest_cookie_expires: None,
                allow_domains: vec![host.into(), "idp.example".into()],
                last_refresh: None,
                writeback_abandoned: None,
            },
        )
        .unwrap();
    }
    let dir = scratch_dir(home.path()).unwrap();
    let export = export_path(dir.path());
    std::fs::write(
        &export,
        br#"{"cookies":[{"name":"sso","value":"SENTINEL","domain":".idp.example","path":"/"}],"origins":[{"origin":"https://a.example.com","localStorage":[{"name":"userToken","value":"newA"}]},{"origin":"https://b.example.com","localStorage":[{"name":"userToken","value":"newB"}]}]}"#,
    )
    .unwrap();
    let (da, db) = (
        vec!["a.example.com".to_string(), "idp.example".to_string()],
        vec!["b.example.com".to_string(), "idp.example".to_string()],
    );
    let targets = [
        split::Target {
            site: "a",
            domains: &da,
        },
        split::Target {
            site: "b",
            domains: &db,
        },
    ];
    let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();

    let got = refresh_profiles(
        home.path(),
        &targets,
        Vec::new(),
        &export,
        now,
        &store,
        soon(),
    );

    for (site, outcome) in &got {
        assert_eq!(outcome.as_ref().unwrap(), &Outcome::Refreshed, "{site}");
    }
    let saved_a = read_state(&paths::profile_state(home.path(), "a"), &store).unwrap();
    let saved_a = String::from_utf8(saved_a).unwrap();
    assert!(saved_a.contains("newA") && !saved_a.contains("newB") && !saved_a.contains("sso"));
    let notices = crate::abandon::take_notices(home.path(), &["a".into(), "b".into()]);
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(notices[0].contains("idp.example") && notices[0].contains("\"b\""));
    assert!(notices[1].contains("idp.example") && notices[1].contains("\"a\""));
    assert!(notices.iter().all(|n| !n.contains("SENTINEL")));
    assert!(!export.exists(), "plaintext export must be removed");
}

const NEW: &[u8] = br#"{"cookies":[],"origins":[{"origin":"https://chat.example.com","localStorage":[{"name":"userToken","value":"new"}]}]}"#;

/// Two sessions load the same profile; B writes back first. A's export is
/// older than what B saved, so A must not overwrite it: the newer copy is
/// kept and A's loss is reported once on next use.
#[test]
fn two_concurrent_sessions_the_second_write_back_keeps_the_newer_copy() {
    let home = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    setup(home.path(), &store);
    let (a, _) = crate::pending::load(home.path(), "ex", &store, far()).unwrap();
    let (b, _) = crate::pending::load(home.path(), "ex", &store, far()).unwrap();
    let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let dir = scratch_dir(home.path()).unwrap();
    let export = export_path(dir.path());

    std::fs::write(&export, NEW).unwrap();
    let got_b = refresh_profile(home.path(), "ex", Some(b), &export, now, &store, soon());
    assert_eq!(got_b.unwrap(), Outcome::Refreshed);

    let stale = br#"{"cookies":[],"origins":[{"origin":"https://chat.example.com","localStorage":[{"name":"userToken","value":"stale"}]}]}"#;
    std::fs::write(&export, stale).unwrap();
    let got_a = refresh_profile(home.path(), "ex", Some(a), &export, now, &store, soon());
    assert!(matches!(got_a.unwrap(), Outcome::Skipped(_)));

    assert_eq!(
        read_state(&paths::profile_state(home.path(), "ex"), &store).unwrap(),
        NEW,
        "the newer write-back survives"
    );
    let notices = crate::abandon::take_notices(home.path(), &["ex".into()]);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("newer copy was kept"), "{notices:?}");
    assert!(crate::abandon::take_notices(home.path(), &["ex".into()]).is_empty());
}

/// The person logs in again (`mur browser auth`) while a session is still
/// open; the session's end must not undo that login.
#[test]
fn a_re_login_while_a_session_is_open_is_not_overwritten() {
    let home = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    setup(home.path(), &store);
    let (open, _) = crate::pending::load(home.path(), "ex", &store, far()).unwrap();
    let relogin = br#"{"cookies":[],"origins":[{"origin":"https://chat.example.com","localStorage":[{"name":"userToken","value":"relogin"}]}]}"#;
    write_state(&paths::profile_state(home.path(), "ex"), relogin, &store).unwrap();

    let dir = scratch_dir(home.path()).unwrap();
    let export = export_path(dir.path());
    std::fs::write(&export, NEW).unwrap();
    let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let got = refresh_profile(home.path(), "ex", Some(open), &export, now, &store, soon());

    assert!(matches!(got.unwrap(), Outcome::Skipped(_)));
    assert_eq!(
        read_state(&paths::profile_state(home.path(), "ex"), &store).unwrap(),
        relogin
    );
    assert!(!export.exists());
}

/// A write-back whose profile stays locked past its deadline skips, leaves
/// its marker, and the next use says the login may not have been saved.
#[test]
fn a_lock_timeout_is_reported_on_next_use() {
    let home = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    setup(home.path(), &store);
    let (mine, _) = crate::pending::load(home.path(), "ex", &store, far()).unwrap();
    let other = crate::abandon::lock(home.path(), "ex", far())
        .unwrap()
        .unwrap();
    let dir = scratch_dir(home.path()).unwrap();
    let export = export_path(dir.path());
    std::fs::write(&export, NEW).unwrap();
    let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let short = Instant::now() + Duration::from_millis(100);

    let got = refresh_profile(home.path(), "ex", Some(mine), &export, now, &store, short);

    assert!(matches!(got.unwrap(), Outcome::Skipped(_)));
    drop(other);
    let notices = crate::abandon::take_notices(home.path(), &["ex".into()]);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("was busy"), "{notices:?}");
}

/// One shared deadline across a merged write-back: a first profile held by
/// another writer must not push the second past the stop window.
#[test]
fn a_busy_first_profile_does_not_spend_the_next_profiles_time() {
    let home = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    for site in ["a", "b"] {
        std::fs::create_dir_all(paths::profile_dir(home.path(), site)).unwrap();
        write_state(&paths::profile_state(home.path(), site), OLD, &store).unwrap();
        auth::write_meta(
            &paths::profile_meta(home.path(), site),
            &auth::ProfileMeta {
                url: format!("https://{site}.example.com/login"),
                last_auth: DateTime::from_timestamp(1_600_000_000, 0).unwrap(),
                earliest_cookie_expires: None,
                allow_domains: vec![format!("{site}.example.com")],
                last_refresh: None,
                writeback_abandoned: None,
            },
        )
        .unwrap();
    }
    let held = [
        crate::abandon::lock(home.path(), "a", far())
            .unwrap()
            .unwrap(),
        crate::abandon::lock(home.path(), "b", far())
            .unwrap()
            .unwrap(),
    ];
    let dir = scratch_dir(home.path()).unwrap();
    let export = export_path(dir.path());
    std::fs::write(&export, br#"{"cookies":[],"origins":[]}"#).unwrap();
    let (da, db) = (
        vec!["a.example.com".to_string()],
        vec!["b.example.com".to_string()],
    );
    let targets = [
        split::Target {
            site: "a",
            domains: &da,
        },
        split::Target {
            site: "b",
            domains: &db,
        },
    ];
    let budget = Duration::from_millis(300);
    let t0 = Instant::now();
    let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();

    let got = refresh_profiles(
        home.path(),
        &targets,
        Vec::new(),
        &export,
        now,
        &store,
        t0 + budget,
    );

    assert!(
        t0.elapsed() < budget + Duration::from_millis(200),
        "{:?}",
        t0.elapsed()
    );
    assert!(
        got.iter()
            .all(|(_, o)| matches!(o, Ok(Outcome::Skipped(_))))
    );
    drop(held);
}

/// End to end at agent stop (#2360 review): the agent closes the proxy's
/// stdin, the browser takes almost all of `EXPORT_TIMEOUT` to export, and
/// another writer holds the profile lock for part of the write-back. The
/// login must still be saved, and the whole stop must end within
/// `EXPORT_TIMEOUT + WRITEBACK_BUDGET` — which `mur-core` asserts fits the
/// runtime's MCP stop slice before its SIGTERM.
#[tokio::test(flavor = "multi_thread")]
async fn a_slow_export_and_a_contended_lock_still_save_within_the_stop_window() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};

    let home = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    setup(home.path(), &store);
    let (loaded, _) = crate::pending::load(home.path(), "ex", &store, far()).unwrap();
    let dir = scratch_dir(home.path()).unwrap();
    let export = export_path(dir.path());

    let export_delay = EXPORT_TIMEOUT - Duration::from_millis(500);
    let (agent_w, agent_in) = duplex(64 * 1024);
    let (agent_out, _agent_r) = duplex(64 * 1024);
    let (server_in, srv_r) = duplex(64 * 1024);
    let (mut srv_w, server_out) = duplex(64 * 1024);
    // A browser that answers the export only after `export_delay`.
    tokio::spawn(async move {
        let mut lines = BufReader::new(srv_r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            let code = v["params"]["arguments"]["code"]
                .as_str()
                .unwrap_or_default();
            let Some(at) = code.find("path: ") else {
                continue;
            };
            let mut de = serde_json::Deserializer::from_str(&code[at + 6..]).into_iter();
            let path: String = de.next().unwrap().unwrap();
            tokio::time::sleep(export_delay).await;
            std::fs::write(path, NEW).unwrap();
            let resp = serde_json::json!({"jsonrpc":"2.0","id":v["id"],"result":{"content":[]}});
            srv_w
                .write_all(format!("{resp}\n").as_bytes())
                .await
                .unwrap();
        }
    });
    struct Pass;
    impl crate::proxy::Hook for Pass {
        async fn on_request(
            &mut self,
            req: crate::proxy::Request,
            _down: &crate::proxy::Downstream,
        ) -> crate::proxy::Decision {
            crate::proxy::Decision::Forward(req)
        }
        async fn on_response(
            &mut self,
            _req: &crate::proxy::Request,
            resp: serde_json::Value,
            _down: &crate::proxy::Downstream,
        ) -> serde_json::Value {
            resp
        }
    }

    let t0 = Instant::now();
    drop(agent_w); // the runtime's graceful stop: stdin EOF
    crate::proxy::run_io_inner(
        agent_in,
        agent_out,
        server_in,
        server_out,
        Pass,
        Some(&export),
    )
    .await
    .unwrap();
    assert!(export.is_file(), "the slow export must not be abandoned");

    // Another writer holds the lock for half the write-back budget.
    let other = crate::abandon::lock(home.path(), "ex", far())
        .unwrap()
        .unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(WRITEBACK_BUDGET / 2);
        drop(other);
    });
    let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let deadline = Instant::now() + WRITEBACK_BUDGET;
    let got = refresh_profile(
        home.path(),
        "ex",
        Some(loaded),
        &export,
        now,
        &store,
        deadline,
    );
    release.join().unwrap();

    assert_eq!(got.unwrap(), Outcome::Refreshed);
    assert_eq!(
        read_state(&paths::profile_state(home.path(), "ex"), &store).unwrap(),
        NEW
    );
    let took = t0.elapsed();
    assert!(took <= EXPORT_TIMEOUT + WRITEBACK_BUDGET, "{took:?}");
    assert!(crate::abandon::take_notices(home.path(), &["ex".into()]).is_empty());
}
