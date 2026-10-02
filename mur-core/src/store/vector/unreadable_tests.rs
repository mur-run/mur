use super::*;
use crate::store::vector::{LanceDbStore, SearchFilter, VectorStore};
use std::path::Path;
use tempfile::TempDir;

const DIM: i32 = 8;

fn chunk(id: &str) -> crate::store::vector::EmbeddedChunk {
    crate::store::vector::EmbeddedChunk {
        chunk_id: id.into(),
        source_id: "skill".into(),
        external_id: id.into(),
        ordinal: 0,
        text: "t".into(),
        heading_path: vec![],
        char_range: (0, 1),
        updated_at: chrono::Utc::now(),
        embedding: vec![0.5_f32; DIM as usize],
    }
}

/// Build a populated `sources` table, then overwrite every manifest under
/// `_versions` with garbage — the shape of a table LanceDB cannot decode.
async fn corrupted_sources(dir: &Path) {
    let store = LanceDbStore::open(dir, DIM).await.unwrap();
    store.upsert(&[chunk("a")]).await.unwrap();
    drop(store);
    let versions = dir.join("sources.lance").join("_versions");
    let mut n = 0;
    for entry in std::fs::read_dir(&versions).unwrap() {
        let p = entry.unwrap().path();
        if p.extension().is_some_and(|e| e == "manifest") {
            std::fs::write(&p, b"not a lance manifest").unwrap();
            n += 1;
        }
    }
    assert!(n > 0, "fixture must corrupt at least one manifest");
}

fn find_store_error(err: &anyhow::Error) -> Option<&VectorStoreError> {
    err.chain()
        .find_map(|c| c.downcast_ref::<VectorStoreError>())
}

#[tokio::test]
async fn garbage_manifest_is_unreadable_not_panic() {
    let tmp = TempDir::new().unwrap();
    corrupted_sources(tmp.path()).await;

    let store = LanceDbStore::open(tmp.path(), DIM).await.unwrap();
    let err = VectorStore::search(&store, &[0.5; DIM as usize], 3, &SearchFilter::default())
        .await
        .expect_err("corrupt table must not search cleanly");

    match find_store_error(&err) {
        Some(VectorStoreError::Unreadable { table, .. }) => assert_eq!(table, "sources"),
        other => panic!("expected Unreadable, got {other:?} / {err:#}"),
    }
    // Every read entry point on the trait classifies the same way.
    assert!(is_unreadable(&store.count(None).await.unwrap_err()));
    assert!(is_unreadable(
        &store.list_external_ids("skill").await.unwrap_err()
    ));
}

#[tokio::test]
async fn rebuild_hint_names_the_owner_command() {
    let tmp = TempDir::new().unwrap();
    corrupted_sources(tmp.path()).await;
    let store = LanceDbStore::open(tmp.path(), DIM).await.unwrap();
    let err = store.count(None).await.unwrap_err();

    let hinted = with_rebuild_hint(err, "skill embedding", "mur skill reindex-vec");
    let shown = format!("{hinted:#}");
    assert!(
        shown.contains("rebuild it with `mur skill reindex-vec`"),
        "{shown}"
    );
    assert!(shown.contains("skill embedding index"), "{shown}");
    // The original cause stays in the chain for debugging.
    assert!(find_store_error(&hinted).is_some());
}

#[tokio::test]
async fn hint_is_not_added_to_unrelated_errors() {
    let err = anyhow::anyhow!("embedding endpoint unreachable");
    let out = with_rebuild_hint(err, "skill embedding", "mur skill reindex-vec");
    assert!(!format!("{out:#}").contains("rebuild"));
}

#[cfg(unix)]
#[tokio::test]
async fn permission_denied_is_io_without_hint() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    {
        let store = LanceDbStore::open(tmp.path(), DIM).await.unwrap();
        store.upsert(&[chunk("a")]).await.unwrap();
    }
    let table_dir = tmp.path().join("sources.lance");
    std::fs::set_permissions(&table_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Root ignores mode bits; the case cannot be produced there.
    if std::fs::read_dir(&table_dir).is_ok() {
        std::fs::set_permissions(&table_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipping: running with privileges that bypass mode bits");
        return;
    }

    let store = LanceDbStore::open(tmp.path(), DIM).await.unwrap();
    let result = store.count(None).await;
    std::fs::set_permissions(&table_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    let err = result.expect_err("unreadable dir must fail");
    match find_store_error(&err) {
        Some(VectorStoreError::Io { path, .. }) => assert_eq!(path, &table_dir),
        other => panic!("expected Io, got {other:?} / {err:#}"),
    }
    let hinted = with_rebuild_hint(err, "skill embedding", "mur skill reindex-vec");
    assert!(!format!("{hinted:#}").contains("rebuild it with"));
}

#[tokio::test]
async fn drop_if_unreadable_clears_way_for_rebuild() {
    let tmp = TempDir::new().unwrap();
    corrupted_sources(tmp.path()).await;
    let db = lancedb::connect(tmp.path().to_str().unwrap())
        .execute()
        .await
        .unwrap();
    assert!(drop_if_unreadable(&db, "sources").await.unwrap());

    // The store can now recreate and use the table.
    let store = LanceDbStore::open(tmp.path(), DIM).await.unwrap();
    store.upsert(&[chunk("b")]).await.unwrap();
    assert_eq!(store.count(None).await.unwrap(), 1);
}

#[tokio::test]
async fn drop_if_unreadable_keeps_healthy_and_missing_tables() {
    let tmp = TempDir::new().unwrap();
    {
        let store = LanceDbStore::open(tmp.path(), DIM).await.unwrap();
        store.upsert(&[chunk("a")]).await.unwrap();
    }
    let db = lancedb::connect(tmp.path().to_str().unwrap())
        .execute()
        .await
        .unwrap();
    assert!(!drop_if_unreadable(&db, "sources").await.unwrap());
    assert!(!drop_if_unreadable(&db, "patterns").await.unwrap());
    let store = LanceDbStore::open(tmp.path(), DIM).await.unwrap();
    assert_eq!(store.count(None).await.unwrap(), 1);
}

#[tokio::test]
async fn pattern_rebuild_replaces_unreadable_table() {
    let tmp = TempDir::new().unwrap();
    // Seed a `sources`-shaped corrupt table under the patterns name by
    // renaming: the rebuild must not care what the old contents were.
    corrupted_sources(tmp.path()).await;
    std::fs::rename(
        tmp.path().join("sources.lance"),
        tmp.path().join("patterns.lance"),
    )
    .unwrap();

    let store = LanceDbStore::open(tmp.path(), DIM).await.unwrap();
    let err = store
        .search(&[0.5; DIM as usize], 3, None)
        .await
        .unwrap_err();
    assert!(is_unreadable(&err), "{err:#}");

    store
        .build_unified_index(&[], &[])
        .await
        .expect("rebuild must replace an unreadable table");
    assert!(store.search(&[0.5; DIM as usize], 3, None).await.is_ok());
}
