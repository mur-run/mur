//! Multimodal drag-drop / paste (D3) commands. Moved verbatim from `commands.rs`.

use anyhow::Context;
use mur_common::multimodal::MultimodalArtifact;

use super::err;
use super::onboarding::onboarding_agent_dir;
use crate::multimodal::pipeline::{MultimodalPipeline, PipelineInput};

// ─── D3 multimodal drag-drop / paste (M3.6) ────────────────────────
//
// Two helpers (`_impl`) back the `#[tauri::command]` wrappers so they
// can be unit-tested without a webview. Caps are enforced *before*
// any pipeline work runs:
//
//   * `MAX_FILES_PER_DROP` — fail-fast on glob-drop accidents
//   * `MAX_TOTAL_BYTES_PER_DROP` — keep one drop bounded
//
// After caps the pipeline does HEIC normalize, sandboxed decode, OCR,
// Unicode scrub, provenance ledger entry. Step 1 (dedupe) lives in
// `main.rs`'s `on_window_event`; step 7 (`untrusted_image_text`) and
// step 9 (turn-flag) land in M3.8 with the `B0SafetyHook`.

const MAX_FILES_PER_DROP: usize = 10;
const MAX_TOTAL_BYTES_PER_DROP: u64 = 30 * 1024 * 1024;

pub async fn multimodal_drop_impl(
    agent: &str,
    paths: Vec<std::path::PathBuf>,
    turn_id: u64,
) -> anyhow::Result<Vec<MultimodalArtifact>> {
    if paths.len() > MAX_FILES_PER_DROP {
        anyhow::bail!(
            "multimodal_drop: max 10 files per drop, got {}",
            paths.len()
        );
    }
    let mut total_bytes = 0u64;
    for p in &paths {
        let m = std::fs::metadata(p).with_context(|| format!("metadata {}", p.display()))?;
        total_bytes += m.len();
    }
    if total_bytes > MAX_TOTAL_BYTES_PER_DROP {
        anyhow::bail!("multimodal_drop: total > 30 MB ({total_bytes} bytes)");
    }
    let agent_home = onboarding_agent_dir(agent)?;
    let pipeline = MultimodalPipeline::new(agent_home, turn_id);
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let a = pipeline
            .process(PipelineInput::Path {
                path: p,
                source: "user_drop".into(),
            })
            .await?;
        out.push(a);
    }
    Ok(out)
}

#[tauri::command]
pub async fn multimodal_drop(
    agent: String,
    paths: Vec<std::path::PathBuf>,
    turn_id: u64,
) -> Result<Vec<MultimodalArtifact>, String> {
    multimodal_drop_impl(&agent, paths, turn_id)
        .await
        .map_err(err)
}

// ─── M3.6.2 — `multimodal_paste` (clipboard → pipeline) ────────────
//
// Two-tier clipboard reader:
//   1. `MUR_TEST_CLIPBOARD_IMAGE` env var (test bypass — points at a
//      file whose bytes we treat as the clipboard image)
//   2. real Tauri clipboard plugin (TODO — `tauri-plugin-clipboard-manager`
//      gates the image accessor behind platform-specific code paths
//      which we wire in M3.7+/M3.9 alongside the React surface)
//
// If neither yields bytes we surface a `clipboard has no image` error
// so the React side can show a polite message.

pub async fn multimodal_paste_impl(
    agent: &str,
    turn_id: u64,
) -> anyhow::Result<Vec<MultimodalArtifact>> {
    let bytes = read_clipboard_image_for_test()
        .or_else(read_clipboard_image_via_plugin)
        .ok_or_else(|| anyhow::anyhow!("clipboard has no image"))?;
    if bytes.len() as u64 > MAX_TOTAL_BYTES_PER_DROP {
        anyhow::bail!("clipboard image > 30 MB");
    }
    let agent_home = onboarding_agent_dir(agent)?;
    let pipeline = MultimodalPipeline::new(agent_home, turn_id);
    let a = pipeline
        .process(PipelineInput::Bytes {
            bytes,
            mime_hint: "image/png".into(),
            source: "user_paste".into(),
        })
        .await?;
    Ok(vec![a])
}

#[tauri::command]
pub async fn multimodal_paste(
    agent: String,
    turn_id: u64,
) -> Result<Vec<MultimodalArtifact>, String> {
    multimodal_paste_impl(&agent, turn_id).await.map_err(err)
}

fn read_clipboard_image_for_test() -> Option<Vec<u8>> {
    std::env::var("MUR_TEST_CLIPBOARD_IMAGE")
        .ok()
        .and_then(|p| std::fs::read(p).ok())
}

fn read_clipboard_image_via_plugin() -> Option<Vec<u8>> {
    // Real Tauri clipboard wiring is platform-specific (the manager
    // plugin's image accessor differs across macOS/Linux/Windows).
    // For M3.6.2 we ship the test bypass; the production path is a
    // TODO tracked alongside the M3.6 milestone (lands with M3.7
    // when the React side wires up the paste handler).
    None
}
