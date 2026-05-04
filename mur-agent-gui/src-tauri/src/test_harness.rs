//! Pure-Rust test harness for Track C3 share channels.
//!
//! Building a real `tauri::test::mock_builder()` requires the full
//! Tauri runtime + a frontend bundle, which is overkill for verifying
//! that a deep-link URL parses into a [`SharePayload`] and reaches the
//! ingestor. Instead, [`MockApp`] threads a [`RecordingIngestor`] (a
//! `Mutex<Vec<SharePayload>>` recorder) through the same
//! `parse_share_url` → `SendIngestor::ingest` path the production
//! `app.deep_link().on_open_url(...)` callback will use.
//!
//! `lib.rs::setup` integration with `tauri-plugin-deep-link` lands in a
//! follow-up so we can iterate on the parser + harness independently.
//!
//! Always compiled into the lib (so integration tests can reach it)
//! but `#[doc(hidden)]` to discourage production callers — the bin
//! crate (`main.rs`) doesn't reference these types.

#![doc(hidden)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::send::{SendIngestor, SharePayload, ShareEmitter, url_scheme};

/// Counts emit-received calls and stores the payloads. Callers swap
/// this in for the production `tauri::AppHandle`-backed emitter.
#[derive(Default)]
pub struct CountingEmitter {
    pub emitted: Mutex<Vec<SharePayload>>,
}

impl ShareEmitter for CountingEmitter {
    fn emit_received(&self, payload: &SharePayload) -> anyhow::Result<()> {
        self.emitted
            .lock()
            .expect("CountingEmitter mutex poisoned")
            .push(payload.clone());
        Ok(())
    }
}

/// Vec-backed `SendIngestor` used by tests to assert which payloads
/// the URL scheme parser produced. Wraps the real `DefaultIngestor`'s
/// behavior would couple tests to filesystem / multimodal pipeline,
/// so the recorder skips the `process_*_text` / `process_artifact`
/// hop and just stores the payload. Channel-level tests own the
/// payload-shape assertion; the multimodal-pipeline → B0 chain is
/// covered separately in M-c3.0 tests.
#[derive(Default)]
pub struct RecordingIngestor {
    pub payloads: Mutex<Vec<SharePayload>>,
}

#[async_trait::async_trait]
impl SendIngestor for RecordingIngestor {
    async fn ingest(&self, payload: SharePayload) -> anyhow::Result<()> {
        self.payloads
            .lock()
            .expect("RecordingIngestor mutex poisoned")
            .push(payload);
        Ok(())
    }
}

/// Test-only stand-in for the Tauri shell.
///
/// `simulate_open_url` is the seam that production code will reach
/// from `tauri-plugin-deep-link`'s `on_open_url` callback once it's
/// wired into `lib.rs::setup`. The harness drives that seam directly
/// to keep the test free of Tauri runtime dependencies.
pub struct MockApp {
    /// Per-agent slug; the parser rejects URLs targeting a different
    /// agent so we plumb the slug through here.
    pub slug: String,
    /// Mirrors the production `agent_home` directory contract even
    /// though `RecordingIngestor` doesn't write to disk — callers may
    /// want to assert downstream artifacts in stacked harnesses.
    pub agent_home: PathBuf,
    pub ingestor: Arc<RecordingIngestor>,
}

impl MockApp {
    pub fn new(agent_home: impl AsRef<Path>, slug: impl Into<String>) -> Self {
        Self {
            slug: slug.into(),
            agent_home: agent_home.as_ref().to_path_buf(),
            ingestor: Arc::new(RecordingIngestor::default()),
        }
    }

    /// Drive the deep-link URL through the same parse → ingest path
    /// the production `on_open_url` callback uses. Errors fail the
    /// test rather than being swallowed (production silently logs
    /// invalid URLs; the harness surfaces them).
    pub async fn simulate_open_url(&self, url: &str) -> anyhow::Result<()> {
        let payload = url_scheme::parse_share_url(url, &self.slug)?;
        self.ingestor.ingest(payload).await
    }

    /// Snapshot the recorded payloads. Returns an owned clone so
    /// callers can iterate without holding the mutex.
    pub fn captured_payloads(&self) -> Vec<SharePayload> {
        self.ingestor
            .payloads
            .lock()
            .expect("RecordingIngestor mutex poisoned")
            .clone()
    }
}

/// Convenience constructor matching the plan's `mock_app(home, slug)`
/// signature. Async because the production analogue (a
/// `tauri::Builder::default().setup(...)`) is async.
pub async fn mock_app(agent_home: impl AsRef<Path>, slug: impl Into<String>) -> MockApp {
    MockApp::new(agent_home, slug)
}
