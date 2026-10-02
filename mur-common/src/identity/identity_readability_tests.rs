use super::*;

/// The handoff crosses a pipe as ONE line and must rebuild the same key.
///
/// Both halves matter: the receiver does `read_line`, so a payload that
/// could carry a newline would arrive truncated and silently unsigned; and
/// a rebuilt identity that is not bit-identical signs events no verifier
/// accepts, which reads as tampering rather than as a bug.
#[test]
fn a_signing_handoff_round_trips_to_the_same_key_on_one_line() {
    let id = AgentIdentity::generate();
    let line = serde_json::to_string(&SigningHandoff {
        agent: "mur".into(),
        key_version: 3,
        secret: id.secret_bytes_for_handoff(),
    })
    .expect("serialize handoff");
    assert!(!line.contains('\n'), "must survive read_line: {line}");

    let back: SigningHandoff = serde_json::from_str(&line).expect("parse handoff");
    assert_eq!(back.agent, "mur");
    assert_eq!(back.key_version, 3);
    assert_eq!(
        AgentIdentity::from_secret_bytes(&back.secret).verifying_key_bytes(),
        id.verifying_key_bytes(),
        "the child must sign as the same agent, not a lookalike"
    );
}

/// A key that exists but cannot be read must NOT report as absent.
///
/// `Path::exists()` answers false for any stat failure, so before this the
/// two were indistinguishable and every caller's "no key yet" branch ran
/// when the truth was "you may not read this key" — which is exactly what
/// a sandbox deny produces.
#[cfg(unix)]
#[test]
fn an_unreadable_key_is_denied_not_notfound() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    AgentIdentity::generate().save(dir.path()).unwrap();
    let key = dir.path().join("identity.key");

    // Precondition: readable right now, so the assert below is about the
    // permission change and nothing else.
    assert!(AgentIdentity::load(dir.path()).is_ok());

    // Case 1 — the file is unreadable but still STATtable (chmod 000).
    // `exists()` says true here, so this exercises the read mapping.
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = AgentIdentity::load(dir.path()).unwrap_err();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        matches!(unreadable, IdentityError::Denied(_)),
        "an unreadable key must be Denied, got {unreadable:?}"
    );

    // Case 2 — the file cannot even be STATted, because the directory
    // holding it is not searchable. THIS is what a sandbox deny looks
    // like, and it is the case `Path::exists()` gets wrong: it answers
    // false, so the old code reported NotFound and every caller's
    // "no key yet" branch ran.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
    let unstattable = AgentIdentity::load(dir.path()).unwrap_err();
    let exists_lies = !key.exists();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        exists_lies,
        "precondition: Path::exists() must be answering false here, or this \
             case is not reproducing a sandbox deny"
    );
    assert!(
        matches!(unstattable, IdentityError::Denied(_)),
        "a key that cannot be STATted must be Denied, not NotFound, got {unstattable:?}"
    );
}

/// An agent home's private key moves; its public half does not.
#[test]
fn an_agent_key_moves_and_the_public_half_stays() {
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path();
    let agent = mur.join("agents").join("pm");
    AgentIdentity::generate().save(&agent).unwrap();

    assert!(
        mur.join("keys").join("pm").join("identity.key").exists(),
        "private key must live under keys/"
    );
    assert!(
        !agent.join("identity.key").exists(),
        "the agents tree must hold no private key"
    );
    assert!(
        agent.join("identity.pub").exists(),
        "the public half must stay where peers read it"
    );
}

/// The three callers that pass a directory outside `agents/` must be left
/// exactly as they are — the spec named this as the main correctness risk.
#[test]
fn non_agent_identities_are_not_remapped() {
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path();
    for dir in [
        mur.to_path_buf(),     // host key
        mur.join("commander"), // commander signing identity
        mur.join("publisher"), // skill publisher identity (#1013)
    ] {
        assert_eq!(
            private_key_dir(&dir),
            dir,
            "{} must not be remapped",
            dir.display()
        );
        AgentIdentity::generate().save(&dir).unwrap();
        assert!(
            dir.join("identity.key").exists(),
            "{} lost its key to the remap",
            dir.display()
        );
    }
}

/// A key left in the legacy location no longer loads on its own — and that
/// is the point of step 3. It becomes loadable by MIGRATING it, which is
/// what every agent does at startup before this call.
///
/// This replaces step 1's fallback test. Keeping the fallback would have
/// meant a private key could sit in the agents tree indefinitely, readable
/// to every sibling, with nothing ever saying so.
#[test]
fn a_legacy_key_loads_only_after_migration() {
    let tmp = tempfile::tempdir().unwrap();
    let agent = tmp.path().join("agents").join("legacy");
    std::fs::create_dir_all(&agent).unwrap();
    let id = AgentIdentity::generate();
    std::fs::write(agent.join("identity.key"), id.signing.to_bytes()).unwrap();

    // Before migration: not found at the only location that is consulted.
    assert!(
        matches!(
            AgentIdentity::load(&agent).unwrap_err(),
            IdentityError::NotFound
        ),
        "a key in the legacy location must not be silently honoured"
    );

    // Startup migrates, then loads — the real sequence.
    assert!(migrate_private_key(&agent).unwrap());
    assert_eq!(
        AgentIdentity::load(&agent).unwrap().pubkey_text(),
        id.pubkey_text()
    );
}

/// ...and when both exist, the new location wins, so a completed migration
/// is authoritative even if a stale file is left behind.
#[test]
fn the_new_location_wins_over_a_leftover_legacy_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path();
    let agent = mur.join("agents").join("dual");
    let current = AgentIdentity::generate();
    current.save(&agent).unwrap();
    let stale = AgentIdentity::generate();
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join("identity.key"), stale.signing.to_bytes()).unwrap();

    let loaded = AgentIdentity::load(&agent).unwrap();
    assert_eq!(
        loaded.pubkey_text(),
        current.pubkey_text(),
        "the migrated key must win over the leftover"
    );
}

/// The migration moves the key and leaves nothing behind.
#[test]
fn migration_moves_the_key_out_of_the_agents_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path();
    let agent = mur.join("agents").join("pm");
    std::fs::create_dir_all(&agent).unwrap();
    let id = AgentIdentity::generate();
    std::fs::write(agent.join("identity.key"), id.signing.to_bytes()).unwrap();

    assert!(migrate_private_key(&agent).unwrap());

    assert!(!agent.join("identity.key").exists(), "key left in agents/");
    assert!(mur.join("keys/pm/identity.key").exists());
    assert_eq!(
        AgentIdentity::load(&agent).unwrap().pubkey_text(),
        id.pubkey_text(),
        "the same identity must load after the move"
    );
}

/// Idempotent: a second run finds nothing to do.
#[test]
fn migration_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let agent = tmp.path().join("agents").join("pm");
    AgentIdentity::generate().save(&agent).unwrap(); // already in keys/
    assert!(!migrate_private_key(&agent).unwrap());
    assert!(!migrate_private_key(&agent).unwrap());
}

/// The property that protects an unrecoverable file: when the destination
/// holds a DIFFERENT key, refuse and touch nothing. Picking one silently
/// would change the agent's identity and forge attribution on every channel
/// it has written to.
#[test]
fn migration_refuses_when_the_destination_differs() {
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path();
    let agent = mur.join("agents").join("pm");
    let migrated = AgentIdentity::generate();
    migrated.save(&agent).unwrap();
    let stray = AgentIdentity::generate();
    std::fs::write(agent.join("identity.key"), stray.signing.to_bytes()).unwrap();

    let err = migrate_private_key(&agent).unwrap_err();

    assert!(matches!(err, IdentityError::Exists(_)), "got {err:?}");
    assert_eq!(
        std::fs::read(mur.join("keys/pm/identity.key")).unwrap(),
        migrated.signing.to_bytes().to_vec(),
        "the destination key was modified despite the refusal"
    );
    assert!(
        agent.join("identity.key").exists(),
        "the source was removed despite the refusal"
    );
}

/// An identical leftover copy is cleaned up rather than refused — leaving a
/// private key in the agents tree is the exposure being removed.
#[test]
fn migration_clears_an_identical_leftover() {
    let tmp = tempfile::tempdir().unwrap();
    let agent = tmp.path().join("agents").join("pm");
    let id = AgentIdentity::generate();
    id.save(&agent).unwrap();
    std::fs::write(agent.join("identity.key"), id.signing.to_bytes()).unwrap();

    assert!(!migrate_private_key(&agent).unwrap());
    assert!(!agent.join("identity.key").exists());
}

/// The three non-agent identities must never be migrated.
#[test]
fn migration_skips_non_agent_identities() {
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path();
    for dir in [
        mur.to_path_buf(),
        mur.join("commander"),
        mur.join("publisher"),
    ] {
        AgentIdentity::generate().save(&dir).unwrap();
        assert!(!migrate_private_key(&dir).unwrap());
        assert!(dir.join("identity.key").exists(), "{}", dir.display());
    }
}

/// `save` must never clobber an existing private key.
///
/// This is the mechanism that would have turned #1011 into a loud failure:
/// `mur skill publish` called `save` on a directory that already held the
/// HOST key, and `fs::write` truncated it. There is no `.prev` and no
/// rotation attestation for such a swap, so the old key's signatures stop
/// attributing with nothing to restore from.
#[test]
fn save_refuses_to_overwrite_an_existing_key() {
    let dir = tempfile::tempdir().unwrap();
    let first = AgentIdentity::generate();
    first.save(dir.path()).unwrap();
    let original = std::fs::read(dir.path().join("identity.key")).unwrap();

    let err = AgentIdentity::generate().save(dir.path()).unwrap_err();

    assert!(
        matches!(err, IdentityError::Exists(_)),
        "expected Exists, got {err:?}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("identity.key")).unwrap(),
        original,
        "the existing key was modified despite the refusal"
    );
}

/// ...but a first save into a fresh directory still works, which is every
/// legitimate caller (agent create, export minting a missing key, rekey
/// writing to its scratch dir).
#[test]
fn save_into_an_empty_directory_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let id = AgentIdentity::generate();
    id.save(dir.path()).unwrap();
    assert_eq!(
        AgentIdentity::load(dir.path()).unwrap().pubkey_text(),
        id.pubkey_text()
    );
}

/// ...and a genuinely absent key still reports NotFound, because callers
/// legitimately treat that as "nothing signed yet".
#[test]
fn a_missing_key_is_still_notfound() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        AgentIdentity::load(dir.path()).unwrap_err(),
        IdentityError::NotFound
    ));
}
