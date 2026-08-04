//! Loading a config with legacy conversation fields migrates it on disk, once.
//!
//! NOTE: override MUR_HOME, never HOME. `store::config::config_path()` checks
//! MUR_HOME first precisely because `dirs::home_dir()` ignores HOME on
//! Windows — a test that overrode only HOME would migrate the developer's
//! real ~/.mur/config.yaml on a Windows CI runner.

use std::fs;
use std::sync::Mutex;

// `MUR_HOME` is a process-global env var. Each `#[test]` fn in this file runs
// in its own process under `cargo nextest`, but this lock is cheap insurance
// against a future run under plain `cargo test` (which multiplexes tests onto
// threads within one process) — matches the pattern in
// `mur-core/tests/cmd_hooks_show.rs`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct MurHomeGuard;
impl Drop for MurHomeGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("MUR_HOME") }
    }
}

#[test]
fn load_config_migrates_legacy_fields_and_writes_back() {
    let _lock = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("config.yaml");
    fs::write(
        &cfg_path,
        "conversations:\n  ask:\n    model: llama3:70b\n    ollama_endpoint: http://box.local:11434\n",
    )
    .unwrap();

    unsafe { std::env::set_var("MUR_HOME", tmp.path()) };
    let _guard = MurHomeGuard;

    let cfg = mur_core::store::config::load_config().expect("loads");
    let b = cfg
        .conversations
        .ask
        .backend
        .clone()
        .expect("pinned by migration");
    assert_eq!(b.provider, "ollama");
    assert_eq!(b.model, "llama3:70b");
    assert_eq!(b.endpoint.as_deref(), Some("http://box.local:11434"));

    let on_disk = fs::read_to_string(&cfg_path).unwrap();
    assert!(
        on_disk.contains("backend:"),
        "migration written back:\n{on_disk}"
    );
    assert!(
        !on_disk.contains("ollama_endpoint: http://box.local"),
        "legacy endpoint key removed:\n{on_disk}"
    );
    // Structural check, not substring: the new `backend.model` field is also
    // named "model" and carries the same value, so `contains("model: ...")`
    // would false-positive on the very thing migration is supposed to write.
    let parsed: serde_yaml::Value = serde_yaml::from_str(&on_disk).unwrap();
    assert!(
        parsed["conversations"]["ask"]["model"].is_null(),
        "legacy top-level model key removed:\n{on_disk}"
    );

    // Second load is a no-op: byte-identical file.
    let before = on_disk.clone();
    let _ = mur_core::store::config::load_config().expect("loads again");
    assert_eq!(fs::read_to_string(&cfg_path).unwrap(), before, "idempotent");
}

/// A `backend:` (or `{extractive,abstractive}_backend:`) key that is present
/// but explicitly `null` is the literal state of every real user config
/// today (Task 6 lands after all four override fields have shipped as `null`
/// defaults). `migrate_conversations_yaml` already treats an explicit null as
/// "not yet an override" and pins/clears it same as an absent key — but nothing
/// exercised that path end-to-end through the loader before this test.
#[test]
fn load_config_migrates_explicit_null_backend_keys_and_stays_idempotent() {
    let _lock = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("config.yaml");
    fs::write(
        &cfg_path,
        "\
conversations:
  ask:
    model: llama3:70b
    ollama_endpoint: http://box.local:11434
    backend: null
  compact:
    extractive_model: qwen3.5:4b
    abstractive_model: qwen3.5:4b
    ollama_endpoint: http://localhost:11434
    extractive_backend: null
    abstractive_backend: null
",
    )
    .unwrap();

    unsafe { std::env::set_var("MUR_HOME", tmp.path()) };
    let _guard = MurHomeGuard;

    let cfg = mur_core::store::config::load_config().expect("loads");

    // ask was pointed at a non-default model AND a customized endpoint ->
    // pinned to an explicit ollama backend, overwriting the null.
    let ask_backend = cfg
        .conversations
        .ask
        .backend
        .clone()
        .expect("ask pinned by migration despite starting as `backend: null`");
    assert_eq!(ask_backend.provider, "ollama");
    assert_eq!(ask_backend.model, "llama3:70b");
    assert_eq!(
        ask_backend.endpoint.as_deref(),
        Some("http://box.local:11434")
    );

    // compact's model AND endpoint were both at shipped defaults -> both
    // stages inherit the smart slot (None), not pinned to an empty backend.
    assert!(
        cfg.conversations.compact.extractive_backend.is_none(),
        "extractive stage at shipped defaults must stay None, got {:?}",
        cfg.conversations.compact.extractive_backend
    );
    assert!(
        cfg.conversations.compact.abstractive_backend.is_none(),
        "abstractive stage at shipped defaults must stay None, got {:?}",
        cfg.conversations.compact.abstractive_backend
    );

    let on_disk = fs::read_to_string(&cfg_path).unwrap();
    assert!(
        !on_disk.contains("ollama_endpoint"),
        "legacy endpoint keys removed from both stages:\n{on_disk}"
    );
    assert!(
        !on_disk.contains("extractive_model") && !on_disk.contains("abstractive_model"),
        "legacy compact model keys removed:\n{on_disk}"
    );

    // Second load is a no-op: byte-identical file.
    let before = on_disk.clone();
    let _ = mur_core::store::config::load_config().expect("loads again");
    assert_eq!(fs::read_to_string(&cfg_path).unwrap(), before, "idempotent");
}
