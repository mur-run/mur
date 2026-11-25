use super::*;
use crate::identity::private_key_dir;
use std::path::PathBuf;

fn mur_home() -> PathBuf {
    PathBuf::from("/m")
}

fn home_of(name: &str) -> PathBuf {
    mur_home().join("agents").join(name)
}

#[test]
fn installed_agent_names_a_home_under_agents() {
    let home = home_of("bob");
    assert_eq!(installed_agent(&home, &mur_home()), Some("bob"));
}

#[test]
fn installed_agent_is_none_for_a_cache_home_and_for_a_purged_home() {
    let m = mur_home();
    for not_installed in [
        m.join("cache").join("bob"),
        m.join("agents").join("bob").join("sub"),
        m.join("agents"),
        PathBuf::from("/elsewhere/agents/bob"),
        home_of(".purged.x.1"),
        home_of(".mur.seeding"),
        home_of("-x"),
        home_of("a..b"),
    ] {
        assert_eq!(
            installed_agent(&not_installed, &m),
            None,
            "{}",
            not_installed.display()
        );
    }
}

/// Every string over a small alphabet that holds each character class the
/// name rules care about: letters, both separators, a dot, and a digit.
fn all_names() -> Vec<String> {
    let alphabet = ['a', 'Z', '0', '-', '_', '.'];
    let mut out = vec![String::new()];
    let mut frontier = vec![String::new()];
    for _ in 0..4 {
        let mut next = Vec::new();
        for s in &frontier {
            for c in alphabet {
                next.push(format!("{s}{c}"));
            }
        }
        out.extend(next.iter().cloned());
        frontier = next;
    }
    out
}

#[test]
fn installed_agent_and_private_key_dir_agree_for_every_valid_name() {
    let m = mur_home();
    let mut checked = 0;
    for name in all_names() {
        if crate::agent_name::validate_agent_name(&name).is_err() {
            continue;
        }
        let home = home_of(&name);
        assert_eq!(installed_agent(&home, &m), Some(name.as_str()), "{name:?}");
        assert_eq!(
            private_key_dir(&home),
            m.join("keys").join(&name),
            "{name:?}"
        );
        checked += 1;
    }
    assert!(checked > 100, "only {checked} valid names generated");
    // And a path that is not an installed home is left alone by both.
    let cache = m.join("cache").join("bob");
    assert_eq!(installed_agent(&cache, &m), None);
    assert_eq!(private_key_dir(&cache), cache);
}

fn make_home(mur_home: &Path, name: &str, with_profile: bool) {
    let dir = mur_home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    if with_profile {
        std::fs::write(dir.join("profile.yaml"), b"name: x\n").unwrap();
    }
}

#[test]
fn agent_dirs_skips_purged_and_seeding_directories_and_plain_files() {
    let m = tempfile::tempdir().unwrap();
    make_home(m.path(), "b", true);
    make_home(m.path(), "a", false);
    make_home(m.path(), ".purged.x.1", true);
    make_home(m.path(), ".mur.seeding", true);
    make_home(m.path(), ".mur.seeding.7", true);
    std::fs::write(m.path().join("agents").join("file"), b"").unwrap();
    let names: Vec<String> = agent_dirs(m.path()).map(|(n, _)| n).collect();
    assert_eq!(names, ["a", "b"], "sorted, valid names, directories only");
    let (name, path) = agent_dirs(m.path()).next().unwrap();
    assert_eq!(path, m.path().join("agents").join(name));
}

#[test]
fn agent_dirs_of_a_home_without_agents_is_empty() {
    let m = tempfile::tempdir().unwrap();
    assert_eq!(agent_dirs(m.path()).count(), 0);
}

#[test]
fn installed_agents_also_requires_a_profile() {
    let m = tempfile::tempdir().unwrap();
    make_home(m.path(), "with", true);
    make_home(m.path(), "bare", false);
    make_home(m.path(), ".purged.with.1", true);
    let names: Vec<String> = installed_agents(m.path()).map(|(n, _)| n).collect();
    assert_eq!(names, ["with"]);
}
