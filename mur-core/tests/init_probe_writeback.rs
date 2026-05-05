//! Integration test: when a user picks an oMLX model whose dims were not
//! populated at discovery time (kind=Unknown, dims=None), the probe path
//! in `select_local_embedding` resolves dims via `/v1/embeddings`.
//!
//! This file tests the probe primitive directly (no TTY). The full UX
//! path is covered by the manual smoke checklist.

use mur_core::discovery::{Backend, DiscoveredModel, ModelKind};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn probe_populates_dims_for_unprobed_omlx_pick() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"embedding": vec![0.0f32; 1024]}]
        })))
        .mount(&server)
        .await;

    use mur_core::discovery::omlx::OMlxDiscovery;
    use mur_core::discovery::Discovery;
    let d = OMlxDiscovery::new(server.uri());
    let probe = d
        .probe_embedding("mlx-community/Qwen3-Embedding-0.6B-8bit")
        .await
        .unwrap();
    assert_eq!(probe.dims, 1024);

    // TODO: replace this manual simulation with a headless invocation of
    // select_local_embedding once stdin can be injected (the menu prompt
    // currently blocks on io::stdin().read_line). Until then, the
    // config-write side of the probe path is covered only by the manual
    // smoke checklist.
    //
    // Simulate what select_local_embedding does when dims==None:
    // construct the model, probe, then annotate.
    let mut m = DiscoveredModel {
        id: "mlx-community/Qwen3-Embedding-0.6B-8bit".into(),
        backend: Backend::OMlx,
        kind: ModelKind::Unknown,
        dims: None,
        family: Some("qwen3".into()),
        size_bytes: None,
        probed_at: None,
    };
    if m.dims.is_none() {
        m.dims = Some(probe.dims);
        m.kind = ModelKind::Embedding;
    }
    assert_eq!(m.dims, Some(1024));
    assert_eq!(m.kind, ModelKind::Embedding);
}
