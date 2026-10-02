use super::*;
use mur_common::pattern::*;
use tempfile::TempDir;

const TEST_DIM: i32 = 64;

// Conformance suite — see store/vector/tests.rs
crate::vector_store_conformance!(LanceDbStore, make_store_for_conformance);

async fn make_store_for_conformance() -> LanceDbStore {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap();
    store.ensure_sources_table().await.unwrap();
    // Intentionally leak the TempDir: the LanceDB connection holds an open
    // handle and expects the directory to persist for the lifetime of the
    // `store` (which outlives this function). Conformance tests are few;
    // leaked temp dirs get cleaned by the OS on reboot.
    std::mem::forget(tmp);
    store
}

fn make_pattern(name: &str) -> Pattern {
    Pattern {
        base: mur_common::knowledge::KnowledgeBase {
            schema: 2,
            name: name.into(),
            description: format!("About {}", name),
            content: Content::Plain("test content".into()),
            tier: Tier::Session,
            importance: 0.5,
            confidence: 0.5,
            tags: Tags::default(),
            applies: Applies::default(),
            evidence: Evidence::default(),
            links: Links::default(),
            lifecycle: Lifecycle::default(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ..Default::default()
        },
        kind: None,
        origin: None,
        attachments: vec![],
    }
}

fn make_workflow(name: &str) -> Workflow {
    Workflow {
        base: mur_common::knowledge::KnowledgeBase {
            name: name.into(),
            description: format!("Workflow: {}", name),
            content: Content::Plain("workflow content".into()),
            ..Default::default()
        },
        steps: vec![],
        variables: vec![],
        source_sessions: vec![],
        trigger: String::new(),
        tools: vec![],
        published_version: 0,
        permission: Default::default(),
        schedule: None,
        id: None,
        notify: None,
        requires: vec![],
    }
}

fn random_embedding() -> Vec<f32> {
    (0..TEST_DIM as usize)
        .map(|i| (i as f32 * 0.01).sin())
        .collect()
}

#[tokio::test]
async fn test_build_and_search() {
    let tmp = TempDir::new().unwrap();
    let store = LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap();

    let patterns = vec![
        (make_pattern("pattern-a"), random_embedding()),
        (make_pattern("pattern-b"), {
            let mut v = random_embedding();
            v[0] += 1.0;
            v
        }),
    ];

    store.build_index(&patterns).await.unwrap();

    let results = store.search(&random_embedding(), 5, None).await.unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].name, "pattern-a");
    assert_eq!(results[0].item_type, "pattern");
}

#[tokio::test]
async fn test_empty_index() {
    let tmp = TempDir::new().unwrap();
    let store = LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap();
    let results = store.search(&random_embedding(), 5, None).await.unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn test_rebuild_index() {
    let tmp = TempDir::new().unwrap();
    let store = LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap();

    let patterns = vec![(make_pattern("first"), random_embedding())];
    store.build_index(&patterns).await.unwrap();

    let patterns2 = vec![
        (make_pattern("second"), random_embedding()),
        (make_pattern("third"), {
            let mut v = random_embedding();
            v[0] += 0.5;
            v
        }),
    ];
    store.build_index(&patterns2).await.unwrap();

    let results = store.search(&random_embedding(), 10, None).await.unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.name != "first"));
}

#[test]
fn test_content_with_attachment_descriptions() {
    let mut p = make_pattern("attach-test");
    assert_eq!(
        super::content_with_attachment_descriptions(&p),
        "test content"
    );

    // Add attachments with descriptions
    p.attachments = vec![
        mur_common::pattern::Attachment {
            att_type: mur_common::pattern::AttachmentType::Diagram,
            format: mur_common::pattern::AttachmentFormat::Mermaid,
            path: "attach-test/arch.mermaid".into(),
            description: "System architecture overview".into(),
        },
        mur_common::pattern::Attachment {
            att_type: mur_common::pattern::AttachmentType::Image,
            format: mur_common::pattern::AttachmentFormat::Png,
            path: "attach-test/screen.png".into(),
            description: "Dashboard screenshot".into(),
        },
    ];

    let text = super::content_with_attachment_descriptions(&p);
    assert!(text.contains("test content"));
    assert!(text.contains("System architecture overview"));
    assert!(text.contains("Dashboard screenshot"));
}

#[test]
fn test_content_with_empty_attachment_descriptions() {
    let mut p = make_pattern("empty-desc");
    p.attachments = vec![mur_common::pattern::Attachment {
        att_type: mur_common::pattern::AttachmentType::Diagram,
        format: mur_common::pattern::AttachmentFormat::Mermaid,
        path: "empty-desc/flow.mermaid".into(),
        description: "".into(), // empty description
    }];

    let text = super::content_with_attachment_descriptions(&p);
    // Should not add extra newlines for empty descriptions
    assert_eq!(text, "test content");
}

#[tokio::test]
async fn open_or_create_sources_table_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let store = LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap();
    // First call creates
    store.ensure_sources_table().await.unwrap();
    // Second call is a no-op
    store.ensure_sources_table().await.unwrap();
    // Row count zero
    let c = <LanceDbStore as VectorStore>::count(&store, None)
        .await
        .unwrap();
    assert_eq!(c, 0);
}

#[tokio::test]
async fn sources_upsert_and_search_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let store = LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap();
    store.ensure_sources_table().await.unwrap();

    let now = chrono::Utc::now();
    let mk_chunk = |id: &str, ext: &str, text: &str, embed: Vec<f32>| -> super::EmbeddedChunk {
        super::EmbeddedChunk {
            chunk_id: id.into(),
            source_id: "obsidian:test".into(),
            external_id: ext.into(),
            ordinal: 0,
            text: text.into(),
            heading_path: vec!["Section".into()],
            char_range: (0, text.len()),
            updated_at: now,
            embedding: embed,
        }
    };

    let v_a: Vec<f32> = (0..TEST_DIM as usize)
        .map(|i| (i as f32 * 0.01).sin())
        .collect();
    let v_b: Vec<f32> = (0..TEST_DIM as usize)
        .map(|i| (i as f32 * 0.01).cos())
        .collect();

    <LanceDbStore as super::VectorStore>::upsert(
        &store,
        &[mk_chunk("c1", "doc-a", "alpha text", v_a.clone())],
    )
    .await
    .unwrap();
    <LanceDbStore as super::VectorStore>::upsert(
        &store,
        &[mk_chunk("c2", "doc-b", "bravo text", v_b.clone())],
    )
    .await
    .unwrap();

    let hits = <LanceDbStore as super::VectorStore>::search(
        &store,
        &v_a,
        5,
        &super::SearchFilter::default(),
    )
    .await
    .unwrap();
    assert!(!hits.is_empty(), "expected hits");
    assert_eq!(hits[0].chunk_id, "c1");
    assert_eq!(hits[0].source_id, "obsidian:test");
    assert_eq!(hits[0].external_id, "doc-a");
    assert_eq!(hits[0].heading_path, vec!["Section".to_string()]);
}

#[tokio::test]
async fn sources_list_external_ids_and_count_work() {
    let tmp = TempDir::new().unwrap();
    let store = LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap();
    let now = chrono::Utc::now();
    let zeros = vec![0.0_f32; TEST_DIM as usize];

    let chunks: Vec<super::EmbeddedChunk> = (0..3)
        .map(|i| super::EmbeddedChunk {
            chunk_id: format!("cid-{i}"),
            source_id: "obsidian:test".into(),
            external_id: format!("doc-{i}"),
            ordinal: 0,
            text: "x".into(),
            heading_path: vec![],
            char_range: (0, 1),
            updated_at: now,
            embedding: zeros.clone(),
        })
        .collect();

    <LanceDbStore as super::VectorStore>::upsert(&store, &chunks)
        .await
        .unwrap();

    let ids = <LanceDbStore as super::VectorStore>::list_external_ids(&store, "obsidian:test")
        .await
        .unwrap();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(sorted, vec!["doc-0", "doc-1", "doc-2"]);

    let all = <LanceDbStore as super::VectorStore>::count(&store, None)
        .await
        .unwrap();
    assert_eq!(all, 3);

    let scoped = <LanceDbStore as super::VectorStore>::count(&store, Some("obsidian:test"))
        .await
        .unwrap();
    assert_eq!(scoped, 3);

    let other = <LanceDbStore as super::VectorStore>::count(&store, Some("nope"))
        .await
        .unwrap();
    assert_eq!(other, 0);
}

fn dim_chunk(id: &str, dim: usize) -> super::EmbeddedChunk {
    super::EmbeddedChunk {
        chunk_id: id.into(),
        source_id: "skill".into(),
        external_id: id.into(),
        ordinal: 0,
        text: "t".into(),
        heading_path: vec![],
        char_range: (0, 1),
        updated_at: chrono::Utc::now(),
        embedding: vec![0.5_f32; dim],
    }
}

#[tokio::test]
async fn sources_upsert_rejects_wrong_embedding_len_without_deleting() {
    let tmp = TempDir::new().unwrap();
    let store = LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap();
    <LanceDbStore as super::VectorStore>::upsert(&store, &[dim_chunk("c1", TEST_DIM as usize)])
        .await
        .unwrap();

    let err = <LanceDbStore as super::VectorStore>::upsert(
        &store,
        &[dim_chunk("c1", TEST_DIM as usize * 2)],
    )
    .await;
    assert!(err.is_err(), "mismatched embedding must be rejected");
    let c = <LanceDbStore as super::VectorStore>::count(&store, None)
        .await
        .unwrap();
    assert_eq!(c, 1, "existing row must survive a rejected upsert");
}

#[tokio::test]
async fn sources_upsert_rejects_table_dim_mismatch_without_deleting() {
    let tmp = TempDir::new().unwrap();
    // Table created at 2x dims (e.g. an old 2560-dim index)...
    let old = LanceDbStore::open(tmp.path(), TEST_DIM * 2).await.unwrap();
    <LanceDbStore as super::VectorStore>::upsert(&old, &[dim_chunk("c1", TEST_DIM as usize * 2)])
        .await
        .unwrap();
    drop(old);

    // ...then config changes to TEST_DIM (e.g. 1024).
    let store = LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap();
    let err =
        <LanceDbStore as super::VectorStore>::upsert(&store, &[dim_chunk("c1", TEST_DIM as usize)])
            .await
            .expect_err("dimension mismatch with table must be rejected");
    assert!(
        err.to_string().contains("reindex-vec"),
        "error should say how to fix: {err}"
    );

    let reopened = LanceDbStore::open(tmp.path(), TEST_DIM * 2).await.unwrap();
    let c = <LanceDbStore as super::VectorStore>::count(&reopened, None)
        .await
        .unwrap();
    assert_eq!(c, 1, "existing row must survive a rejected upsert");
}

#[tokio::test]
async fn sources_delete_operations_work() {
    let tmp = TempDir::new().unwrap();
    let store = LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap();
    let now = chrono::Utc::now();
    let zeros = vec![0.0_f32; TEST_DIM as usize];

    let chunks: Vec<super::EmbeddedChunk> = (0..4)
        .map(|i| super::EmbeddedChunk {
            chunk_id: format!("cid-{i}"),
            source_id: if i < 2 {
                "src:a".into()
            } else {
                "src:b".into()
            },
            external_id: format!("doc-{i}"),
            ordinal: 0,
            text: "x".into(),
            heading_path: vec![],
            char_range: (0, 1),
            updated_at: now,
            embedding: zeros.clone(),
        })
        .collect();

    <LanceDbStore as super::VectorStore>::upsert(&store, &chunks)
        .await
        .unwrap();

    <LanceDbStore as super::VectorStore>::delete_by_external_ids(
        &store,
        "src:a",
        &["doc-0".to_string()],
    )
    .await
    .unwrap();
    let remaining_a = <LanceDbStore as super::VectorStore>::list_external_ids(&store, "src:a")
        .await
        .unwrap();
    assert_eq!(remaining_a, vec!["doc-1"]);

    <LanceDbStore as super::VectorStore>::delete_by_source(&store, "src:b")
        .await
        .unwrap();
    let remaining_b = <LanceDbStore as super::VectorStore>::list_external_ids(&store, "src:b")
        .await
        .unwrap();
    assert!(remaining_b.is_empty());

    let still_a = <LanceDbStore as super::VectorStore>::list_external_ids(&store, "src:a")
        .await
        .unwrap();
    assert_eq!(still_a, vec!["doc-1"]);
}

#[tokio::test]
async fn test_unified_index() {
    let tmp = TempDir::new().unwrap();
    let store = LanceDbStore::open(tmp.path(), TEST_DIM).await.unwrap();

    let patterns = vec![(make_pattern("pat-a"), random_embedding())];
    let workflows = vec![(make_workflow("wf-a"), {
        let mut v = random_embedding();
        v[0] += 1.0;
        v
    })];

    store
        .build_unified_index(&patterns, &workflows)
        .await
        .unwrap();

    // Search all
    let results = store.search(&random_embedding(), 10, None).await.unwrap();
    assert_eq!(results.len(), 2);

    // Filter to patterns only
    let pat_results = store
        .search(&random_embedding(), 10, Some("pattern"))
        .await
        .unwrap();
    assert!(pat_results.iter().all(|r| r.item_type == "pattern"));

    // Filter to workflows only
    let wf_results = store
        .search(&random_embedding(), 10, Some("workflow"))
        .await
        .unwrap();
    assert!(wf_results.iter().all(|r| r.item_type == "workflow"));
}
