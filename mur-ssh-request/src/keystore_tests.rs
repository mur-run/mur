use super::*;

use std::os::unix::fs::symlink;

const AGENT: &str = "demo";

/// These tests run the real `ssh-keygen`. Without it they skip, loudly.
fn keygen_missing() -> bool {
    let missing = Command::new(KEYGEN_BIN)
        .arg("-?")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err();
    if missing {
        eprintln!("SKIPPED: {KEYGEN_BIN} is not on PATH");
    }
    missing
}

fn mode(p: &Path) -> u32 {
    fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn generates_once_and_then_returns_the_same_key() {
    if keygen_missing() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let first = generate(home.path(), AGENT).unwrap();
    let bytes = fs::read(&first.private).unwrap();
    let second = generate(home.path(), AGENT).unwrap();
    assert_eq!(second, first);
    assert_eq!(fs::read(&first.private).unwrap(), bytes, "never rewritten");
    assert_eq!(stored(home.path(), AGENT).unwrap(), Some(first.clone()));
    assert!(mur_common::agent::is_fingerprint(&first.fingerprint));
    assert!(
        first
            .public_line
            .ends_with(&format!("{COMMENT_PREFIX}{AGENT}"))
    );
}

#[test]
fn the_key_and_its_directories_are_private() {
    if keygen_missing() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let key = generate(home.path(), AGENT).unwrap();
    let dir = key_dir(home.path(), AGENT).unwrap();
    assert_eq!(key.private, dir.join(KEY_FILE));
    assert_eq!(mode(&key.private), 0o600);
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(dir.parent().unwrap()), 0o700);
    assert_eq!(mode(&home.path().join(SECRETS_DIR)), 0o700);
    // Nothing of the generation is left beside the key.
    let names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| !n.starts_with(PARTIAL_PREFIX)),
        "{names:?}"
    );
}

#[test]
fn no_key_is_none_and_makes_no_directory() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(stored(home.path(), AGENT).unwrap(), None);
    assert!(!home.path().join(SECRETS_DIR).exists());
}

#[test]
fn sweep_removes_a_partial_generation_and_keeps_the_key() {
    if keygen_missing() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let key = generate(home.path(), AGENT).unwrap();
    let dir = key_dir(home.path(), AGENT).unwrap();
    let partial = dir.join(format!("{PARTIAL_PREFIX}xyz"));
    fs::create_dir(&partial).unwrap();
    fs::write(partial.join(KEY_FILE), b"half").unwrap();
    // A link with the partial prefix is left alone, and so is its target.
    let outside = home.path().join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, dir.join(format!("{PARTIAL_PREFIX}link"))).unwrap();

    sweep_partial_older_than(home.path(), AGENT, Duration::ZERO);

    assert!(!partial.exists());
    assert!(outside.is_dir());
    assert_eq!(stored(home.path(), AGENT).unwrap(), Some(key));
}

/// Two setup cards open for one agent: the second card's sweep must not
/// delete the directory the first card's `ssh-keygen` is still writing into.
#[test]
fn sweep_leaves_a_generation_that_may_still_be_running() {
    let home = tempfile::tempdir().unwrap();
    let dir = key_dir(home.path(), AGENT).unwrap();
    create_private_dir(&dir).unwrap();
    let live = dir.join(format!("{PARTIAL_PREFIX}live"));
    fs::create_dir(&live).unwrap();
    fs::write(live.join(KEY_FILE), b"half").unwrap();

    sweep_partial(home.path(), AGENT);

    assert!(live.join(KEY_FILE).exists(), "a fresh partial was swept");
    // Control: the same directory goes once it is older than the minimum age.
    sweep_partial_older_than(home.path(), AGENT, Duration::ZERO);
    assert!(!live.exists());
}

#[test]
fn a_missing_public_half_is_rebuilt() {
    if keygen_missing() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let key = generate(home.path(), AGENT).unwrap();
    let public = pub_path(&key.private);
    fs::remove_file(&public).unwrap();
    let again = stored(home.path(), AGENT).unwrap().unwrap();
    assert_eq!(again.fingerprint, key.fingerprint);
    assert!(public.is_file());
}

#[test]
fn a_link_at_the_key_name_is_refused_and_left_in_place() {
    if keygen_missing() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let real = generate(other.path(), AGENT).unwrap();
    let dir = key_dir(home.path(), AGENT).unwrap();
    fs::create_dir_all(&dir).unwrap();
    let link = dir.join(KEY_FILE);
    symlink(&real.private, &link).unwrap();

    for result in [
        stored(home.path(), AGENT).map(drop),
        generate(home.path(), AGENT).map(drop),
    ] {
        match result {
            Err(KeystoreError::Unusable { path, .. }) => assert_eq!(path, link),
            other => panic!("expected Unusable, got {other:?}"),
        }
    }
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(real.private.is_file());
}

#[test]
fn a_key_others_can_read_is_refused() {
    if keygen_missing() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let key = generate(home.path(), AGENT).unwrap();
    fs::set_permissions(&key.private, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        stored(home.path(), AGENT),
        Err(KeystoreError::Unusable { .. })
    ));
}

#[test]
fn a_public_half_that_is_not_one_key_is_refused() {
    if keygen_missing() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let key = generate(home.path(), AGENT).unwrap();
    let public = pub_path(&key.private);
    for bad in [
        format!("{}\n{}\n", key.public_line, key.public_line),
        "ssh-rsa AAAAB3NzaC1yc2E= other\n".to_string(),
        "ssh-ed25519 not-base64!\n".to_string(),
        String::new(),
    ] {
        fs::write(&public, &bad).unwrap();
        match stored(home.path(), AGENT) {
            Err(KeystoreError::Unusable { path, .. }) => assert_eq!(path, public),
            other => panic!("{bad:?}: expected Unusable, got {other:?}"),
        }
    }
}

#[test]
fn a_bad_agent_name_is_rejected_before_any_directory_is_made() {
    let home = tempfile::tempdir().unwrap();
    for bad in ["../x", "a/b", "..", "", "-x", "a b"] {
        assert!(matches!(
            generate(home.path(), bad),
            Err(KeystoreError::Name(_))
        ));
        assert!(matches!(
            stored(home.path(), bad),
            Err(KeystoreError::Name(_))
        ));
        assert!(matches!(
            unload_and_delete(home.path(), bad),
            Err(KeystoreError::Name(_))
        ));
        sweep_partial(home.path(), bad);
    }
    assert!(!home.path().join(SECRETS_DIR).exists());
}

#[test]
fn racing_generations_end_with_one_key() {
    if keygen_missing() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let path = home.path();
    let keys: Vec<StoredKey> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..4)
            .map(|_| s.spawn(|| generate(path, AGENT).unwrap()))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let on_disk = stored(path, AGENT).unwrap().unwrap();
    for k in &keys {
        assert_eq!(k.fingerprint, on_disk.fingerprint);
        assert_eq!(k.private, on_disk.private);
    }
}

#[test]
fn removing_an_agent_with_no_key_is_fine() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        unload_and_delete(home.path(), AGENT).unwrap(),
        Removal::Absent
    );
}

#[test]
fn removing_deletes_the_directory_even_when_the_key_is_unusable() {
    let home = tempfile::tempdir().unwrap();
    let dir = key_dir(home.path(), AGENT).unwrap();
    fs::create_dir_all(&dir).unwrap();
    let outside = home.path().join("outside");
    fs::write(&outside, b"keep").unwrap();
    symlink(&outside, dir.join(KEY_FILE)).unwrap();
    assert_eq!(
        unload_and_delete(home.path(), AGENT).unwrap(),
        Removal::Removed { unloaded: false }
    );
    assert!(!dir.exists());
    assert_eq!(fs::read(&outside).unwrap(), b"keep");
}

#[test]
fn a_failed_tool_does_not_put_stderr_in_its_message() {
    let home = tempfile::tempdir().unwrap();
    let missing = home.path().join("no-such-key");
    let mut cmd = Command::new(KEYGEN_BIN);
    cmd.arg("-y").arg("-f").arg(&missing);
    match run_ok(cmd, KEYGEN_BIN) {
        Err(e @ KeystoreError::ToolFailed { .. }) => {
            assert!(!e.to_string().contains("no-such-key"), "{e}");
        }
        Err(KeystoreError::Spawn { .. }) => eprintln!("SKIPPED: {KEYGEN_BIN} is not on PATH"),
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn rename_moves_the_key_and_never_replaces_one() {
    if keygen_missing() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    assert!(!rename(home.path(), AGENT, "other").unwrap(), "no key yet");
    let key = generate(home.path(), AGENT).unwrap();
    assert!(rename(home.path(), AGENT, "other").unwrap());
    assert_eq!(stored(home.path(), AGENT).unwrap(), None);
    let moved = stored(home.path(), "other").unwrap().unwrap();
    assert_eq!(moved.fingerprint, key.fingerprint);

    // A second agent's key is not overwritten by a rename onto its name.
    let fresh = generate(home.path(), AGENT).unwrap();
    assert!(rename(home.path(), AGENT, "other").is_err());
    assert_eq!(
        stored(home.path(), AGENT).unwrap().unwrap().fingerprint,
        fresh.fingerprint
    );
    assert_eq!(
        stored(home.path(), "other").unwrap().unwrap().fingerprint,
        key.fingerprint
    );
    assert!(rename(home.path(), "../x", "other").is_err());
}
