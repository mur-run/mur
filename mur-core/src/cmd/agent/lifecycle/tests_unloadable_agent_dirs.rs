use super::describe_unloadable;

/// The message has to say which of the four situations it is, because the
/// remedy differs: a legacy layout wants migrating, a stray directory
/// wants deleting, and either one still holding a signing key wants that
/// key dealt with first.
#[test]
fn the_message_names_a_signing_key_and_a_legacy_layout() {
    let tmp = tempfile::tempdir().unwrap();

    let legacy_with_key = tmp.path().join("Author");
    std::fs::create_dir_all(&legacy_with_key).unwrap();
    std::fs::write(legacy_with_key.join("agent.yaml"), b"name: Author").unwrap();
    std::fs::write(legacy_with_key.join("identity.key"), b"k").unwrap();
    let m = describe_unloadable(&legacy_with_key);
    assert!(m.contains("legacy agent.yaml"), "{m}");
    assert!(m.contains("signing key"), "{m}");

    let stray_with_key = tmp.path().join("orphan");
    std::fs::create_dir_all(&stray_with_key).unwrap();
    std::fs::write(stray_with_key.join("identity.key"), b"k").unwrap();
    let m = describe_unloadable(&stray_with_key);
    assert!(m.contains("signing key"), "{m}");
    assert!(!m.contains("legacy"), "{m}");

    let empty = tmp.path().join("wztest");
    std::fs::create_dir_all(empty.join("outbox")).unwrap();
    let m = describe_unloadable(&empty);
    assert!(m.contains("no profile.yaml"), "{m}");
    assert!(!m.contains("signing key"), "{m}");
}

/// A warning without a command is why the user reaches for `rm -rf` — and
/// `rm -rf` leaves the launcher and the service behind, so the directory
/// comes back at the next login.
#[test]
fn the_message_names_the_command_that_actually_clears_it() {
    let tmp = tempfile::tempdir().unwrap();

    let stray = tmp.path().join("kelp");
    std::fs::create_dir_all(&stray).unwrap();
    let m = describe_unloadable(&stray);
    assert!(
        m.contains("mur agent remove kelp"),
        "the remedy must name the agent, not <name>: {m}"
    );
    assert!(m.contains("launcher and service"), "{m}");
    assert!(m.contains("--purge"), "{m}");
    assert!(
        !m.contains("destroys its signing key"),
        "no key here, so no key warning: {m}"
    );

    // With a key, `--purge` is destructive in a way worth saying out loud.
    let with_key = tmp.path().join("author");
    std::fs::create_dir_all(&with_key).unwrap();
    std::fs::write(with_key.join("identity.key"), b"k").unwrap();
    let m = describe_unloadable(&with_key);
    assert!(m.contains("mur agent remove author"), "{m}");
    assert!(m.contains("destroys its signing key"), "{m}");
}
