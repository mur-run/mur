use super::*;

use mur_common::test_env::EnvGuard;

/// An index whose state files live under a throwaway `MUR_HOME`. The guard is
/// returned so the caller keeps the environment lock (and the override) for
/// the whole test — the real `~/.mur` is never touched.
fn index_in(home: &Path) -> (EnvGuard, CodebaseIndex) {
    let guard = EnvGuard::set([("MUR_HOME", home)]);
    let idx = CodebaseIndex::new("proj", &home.join("proj"));
    assert!(
        idx.meta_path().starts_with(home),
        "meta must live under the temp MUR_HOME, got {}",
        idx.meta_path().display()
    );
    (guard, idx)
}

fn tagged(tag: &str) -> IndexMetadata {
    IndexMetadata {
        last_indexed: tag.into(),
        ..IndexMetadata::default()
    }
}

#[test]
fn save_meta_roundtrips_and_leaves_no_temp_file() {
    let t = tempfile::tempdir().unwrap();
    let (_env, idx) = index_in(t.path());
    idx.save_meta(&tagged("one")).unwrap();
    assert_eq!(idx.load_meta().unwrap().last_indexed, "one");
    let dir = idx.meta_path().parent().unwrap().to_path_buf();
    let leftovers: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");
}

/// A crash between temp write and rename leaves only a stray temp file; the
/// reader must still see the previous, intact meta.
#[test]
fn torn_temp_write_is_never_observable() {
    let t = tempfile::tempdir().unwrap();
    let (_env, idx) = index_in(t.path());
    idx.save_meta(&tagged("old")).unwrap();

    // Simulate the interrupted writer: half a JSON document in the temp file.
    let mut torn = idx.meta_path().into_os_string();
    torn.push(format!(".tmp-{}", std::process::id()));
    std::fs::write(&torn, r#"{"project_path":"/p","fi"#).unwrap();
    assert_eq!(idx.load_meta().unwrap().last_indexed, "old");

    // A subsequent save replaces the target wholesale (and reuses the temp).
    idx.save_meta(&tagged("new")).unwrap();
    assert_eq!(idx.load_meta().unwrap().last_indexed, "new");
    assert!(!Path::new(&torn).exists(), "stale temp must be consumed");
}

/// Concurrent readers hammering while a writer alternates must only ever parse
/// a complete old or new document.
#[test]
fn concurrent_reader_never_sees_a_partial_meta() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let t = tempfile::tempdir().unwrap();
    let (_env, idx) = index_in(t.path());
    let idx = Arc::new(idx);
    let big = |tag: &str| IndexMetadata {
        project_path: "x".repeat(64 * 1024),
        ..tagged(tag)
    };
    idx.save_meta(&big("a")).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let (idx, stop) = (Arc::clone(&idx), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut reads = 0u32;
            while !stop.load(Ordering::Relaxed) {
                let m = idx.load_meta().expect("torn or missing meta observed");
                assert!(matches!(m.last_indexed.as_str(), "a" | "b"));
                reads += 1;
            }
            reads
        })
    };
    for i in 0..200 {
        idx.save_meta(&big(if i % 2 == 0 { "b" } else { "a" }))
            .unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    assert!(reader.join().unwrap() > 0);
}
