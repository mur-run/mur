use super::*;
use mur_browser::auth::{ProfileMeta, write_meta};
use mur_browser::state::{StateKeyStore, read_state, write_state};

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

fn profile(home: &Path, site: &str, host: &str, store: &MemoryStore) {
    std::fs::create_dir_all(paths::profile_dir(home, site)).unwrap();
    let old = format!(
        r#"{{"cookies":[],"origins":[{{"origin":"https://{host}","localStorage":[{{"name":"userToken","value":"old"}}]}}]}}"#
    );
    write_state(&paths::profile_state(home, site), old.as_bytes(), store).unwrap();
    let meta = ProfileMeta {
        url: format!("https://{host}/login"),
        last_auth: chrono::DateTime::from_timestamp(1_600_000_000, 0).unwrap(),
        earliest_cookie_expires: None,
        allow_domains: vec![host.into()],
        last_refresh: None,
        writeback_abandoned: None,
    };
    write_meta(&paths::profile_meta(home, site), &meta).unwrap();
}

/// A session loaded `a` and `b`, but `b` dropped out of the write-back
/// targets (its metadata became unreadable). The merged export must still be
/// split: `b`'s login must not be saved inside `a` (#2263, #2360).
#[test]
fn a_merged_export_with_one_eligible_target_is_still_split() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let store = MemoryStore::default();
    profile(home, "a", "a.example.com", &store);
    profile(home, "b", "b.example.com", &store);
    std::fs::write(paths::profile_meta(home, "b"), "url: [unclosed").unwrap();
    let profiles = ["a".to_string(), "b".to_string()];

    let t = targets(home, &profiles);
    assert_eq!(t.sites.len(), 1, "only `a` is eligible");
    assert!(t.merged, "the session loaded two profiles");

    let dir = mur_browser::refresh::scratch_dir(home).unwrap();
    let export = mur_browser::refresh::export_path(dir.path());
    std::fs::write(
        &export,
        br#"{"cookies":[{"name":"sid","value":"B_SECRET","domain":".b.example.com","path":"/"}],"origins":[{"origin":"https://a.example.com","localStorage":[{"name":"userToken","value":"newA"}]},{"origin":"https://b.example.com","localStorage":[{"name":"userToken","value":"newB"}]}]}"#,
    )
    .unwrap();
    let deadline = Instant::now() + mur_browser::refresh::WRITEBACK_BUDGET;

    write_back(home, &t, Vec::new(), &export, deadline, &store);

    let saved = read_state(&paths::profile_state(home, "a"), &store).unwrap();
    let saved = String::from_utf8(saved).unwrap();
    assert!(saved.contains("newA"), "{saved}");
    assert!(
        !saved.contains("B_SECRET") && !saved.contains("newB"),
        "b's login leaked into a: {saved}"
    );
    assert!(!export.exists(), "plaintext export must be removed");
}

/// A session with one profile still writes its export back whole.
#[test]
fn a_single_profile_session_is_not_merged() {
    let home = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    profile(home.path(), "a", "a.example.com", &store);

    let t = targets(home.path(), &["a".to_string()]);

    assert_eq!(t.sites.len(), 1);
    assert!(!t.merged);
}
