use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use mur_browser::state::StateKeyStore;
use mur_browser_request::Site;

use super::*;

#[derive(Default)]
struct MemoryStore(Mutex<HashMap<String, String>>);
impl StateKeyStore for MemoryStore {
    fn get(&self, account: &str) -> anyhow::Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> anyhow::Result<()> {
        self.0.lock().unwrap().insert(account.into(), value.into());
        Ok(())
    }
}

const STATE: &[u8] = br#"{"cookies":[],"origins":[]}"#;

fn site(s: &str) -> Site {
    Site::parse(s).unwrap()
}

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(2)
}

fn hosts(h: &[&str]) -> Vec<String> {
    h.iter().map(|s| s.to_string()).collect()
}

/// D6: the port is part of the name, so two local sites never collide.
#[test]
fn default_name_is_the_host_and_port_made_path_safe() {
    assert_eq!(
        default_name(&site("http://gitea.twdd.tw:3000")),
        "gitea.twdd.tw-3000"
    );
    assert_eq!(
        default_name(&site("https://gitea.example")),
        "gitea.example"
    );
    assert_ne!(
        default_name(&site("http://localhost:3000")),
        default_name(&site("http://localhost:3001"))
    );
    let v6 = default_name(&site("http://[::1]:8081"));
    mur_browser::paths::validate_name(&v6).unwrap();
    assert_eq!(v6, "__1-8081");
}

#[test]
fn a_fresh_login_saves_the_origin_and_host() {
    let t = tempfile::tempdir().unwrap();
    let plan = login_plan(t.path(), &site("http://gitea.example:3000"), None, false).unwrap();
    assert_eq!(
        plan,
        LoginPlan {
            profile: "gitea.example-3000".into(),
            url: "http://gitea.example:3000/".into(),
            allow_domains: hosts(&["gitea.example"]),
            replace: false,
        }
    );
    let meta = save_login(
        t.path(),
        &plan,
        STATE,
        &MemoryStore::default(),
        chrono::Utc::now(),
        soon(),
    )
    .unwrap();
    assert_eq!(meta.allow_domains, vec!["gitea.example"]);
    assert!(mur_browser::paths::profile_state(t.path(), "gitea.example-3000").is_file());
}

/// D6: never replaced silently, and a replace keeps the old hosts.
#[test]
fn log_in_again_needs_reauth_and_keeps_the_old_domains() {
    let t = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    let first = LoginPlan {
        profile: "g".into(),
        url: "https://gitea.example/".into(),
        allow_domains: hosts(&["gitea.example", "sso.example"]),
        replace: false,
    };
    save_login(t.path(), &first, STATE, &store, chrono::Utc::now(), soon()).unwrap();

    let err = login_plan(t.path(), &site("gitea.example"), Some("g"), false).unwrap_err();
    assert!(err.to_string().contains("--reauth"), "{err:#}");

    let again = login_plan(t.path(), &site("chat.gitea.example"), Some("g"), true).unwrap();
    assert!(again.replace);
    assert_eq!(
        again.allow_domains,
        hosts(&["gitea.example", "sso.example"]),
        "covered host not added"
    );

    let wider = login_plan(t.path(), &site("other.example"), Some("g"), true).unwrap();
    assert_eq!(
        wider.allow_domains,
        hosts(&["gitea.example", "sso.example", "other.example"])
    );
    save_login(t.path(), &again, STATE, &store, chrono::Utc::now(), soon()).unwrap();
}

#[test]
fn reauth_without_a_profile_is_a_fresh_login() {
    let t = tempfile::tempdir().unwrap();
    let plan = login_plan(t.path(), &site("gitea.example"), Some("g"), true).unwrap();
    assert!(!plan.replace);
}

#[test]
fn a_bad_name_is_refused_before_any_browser_opens() {
    let t = tempfile::tempdir().unwrap();
    assert!(login_plan(t.path(), &site("gitea.example"), Some("../x"), false).is_err());
}

#[test]
fn a_held_profile_lock_saves_nothing() {
    let t = tempfile::tempdir().unwrap();
    let plan = login_plan(t.path(), &site("gitea.example"), Some("g"), false).unwrap();
    let _held = mur_browser::abandon::lock(t.path(), "g", soon())
        .unwrap()
        .unwrap();
    let err = save_login(
        t.path(),
        &plan,
        STATE,
        &MemoryStore::default(),
        chrono::Utc::now(),
        Instant::now(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("busy"), "{err:#}");
    assert!(!mur_browser::paths::profile_state(t.path(), "g").exists());
}
