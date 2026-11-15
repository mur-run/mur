use super::*;
use mur_ssh_request::keystore;

const AGENT: &str = "demo";

/// A home with a real dedicated key, or `None` (printed) without ssh-keygen.
fn key() -> Option<(tempfile::TempDir, PathBuf, StoredKey)> {
    let home = tempfile::tempdir().unwrap();
    match keystore::generate(home.path(), AGENT) {
        Ok(k) => {
            let dir = keystore::key_dir(home.path(), AGENT).unwrap();
            Some((home, dir, k))
        }
        Err(KeystoreError::Spawn { tool, .. }) => {
            eprintln!("SKIPPED: {tool} is not on PATH");
            None
        }
        Err(e) => panic!("generate: {e}"),
    }
}

fn runs_left(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with(RUN_DIR_PREFIX))
        .collect()
}

fn blob_of(key: &StoredKey) -> Vec<u8> {
    use base64::Engine as _;
    let b64 = key.public_line.split_whitespace().nth(1).unwrap();
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .unwrap()
}

#[tokio::test]
async fn a_real_private_agent_holds_the_key_and_leaves_no_socket() {
    let Some((_home, dir, key)) = key() else {
        return;
    };
    let d = match start(&dir, &key).await {
        Ok(d) => d,
        Err(StartError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("SKIPPED: {SSH_AGENT_BIN} is not on PATH");
            return;
        }
        Err(e) => panic!("start: {e}"),
    };
    assert_eq!(d.blobs, [blob_of(&key)].into_iter().collect());
    assert!(runs_left(&dir).is_empty(), "run directory removed");
    assert!(!run_dir(&dir).join(SOCKET_NAME).exists());
    // The held connection still answers after the socket is gone.
    assert_eq!(
        listener::identity_blobs(&d.held).await,
        Some(d.blobs.clone())
    );
}

#[tokio::test]
async fn a_spawn_failure_is_an_error_and_cleans_up() {
    let Some((_home, dir, key)) = key() else {
        return;
    };
    let got = start_with(
        &dir,
        &key,
        |_| Err(std::io::Error::other("no ssh-agent")),
        |_, _| panic!("nothing to load into"),
    )
    .await;
    assert!(matches!(got, Err(StartError::Io(_))));
    assert!(runs_left(&dir).is_empty());
}

#[tokio::test]
async fn a_load_failure_is_an_error_and_stops_the_agent() {
    let Some((_home, dir, key)) = key() else {
        return;
    };
    let got = start_with(&dir, &key, spawn_agent, |_, _| {
        Err(KeystoreError::Timeout { tool: "ssh-add" })
    })
    .await;
    match got {
        Err(StartError::Load(_)) => {}
        Err(StartError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("SKIPPED: {SSH_AGENT_BIN} is not on PATH");
        }
        other => panic!("expected a load failure, got {:?}", other.err()),
    }
    assert!(runs_left(&dir).is_empty());
}

#[test]
fn a_run_directory_left_by_a_killed_start_is_swept_and_a_link_not_followed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("k");
    std::fs::create_dir(&dir).unwrap();
    std::fs::create_dir(dir.join("run-1")).unwrap();
    std::fs::write(dir.join("run-1").join(SOCKET_NAME), b"").unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep"), b"keep").unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("run-2")).unwrap();
    std::fs::write(dir.join("id_ed25519"), b"key").unwrap();
    sweep_runs(&dir);
    assert!(runs_left(&dir).is_empty());
    assert!(outside.join("keep").exists(), "link target untouched");
    assert!(dir.join("id_ed25519").exists(), "the key is not a run dir");
}
