use super::*;

/// `mur` drives its whole CLI inside a tokio runtime, so the embedding
/// cleanup in `cmd_remove` must not call `Handle::block_on` — that panics
/// with "Cannot start a runtime from within a runtime", and it panicked
/// AFTER the files were deleted, so the user saw a crash and no
/// confirmation for an operation that had actually succeeded.
#[tokio::test(flavor = "multi_thread")]
async fn remove_inside_a_tokio_runtime_does_not_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("skills").join("probe-skill");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("skill.yaml"),
        "name: probe-skill\nversion: 1.0.0\npublisher: human:test\n\
             description: probe\ncategory: context\ncontent:\n  abstract: a\n  context: b\n",
    )
    .unwrap();

    // SAFETY: nextest runs each test in its own process.
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_HOME", tmp.path());

    cmd_remove("probe-skill").expect("remove must succeed inside a runtime");
    assert!(!dir.exists(), "skill dir should be gone");
}
