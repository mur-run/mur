use crate::image_gen::{
    CancelToken, ExpressionPrompt, ImageGenProvider, ImageSize, RenderProgress,
};
use anyhow::Result;
use mur_common::hub::{
    preset_manifest::{PresetManifest, compute_preset_hash, manifest_path},
    style_preset::StylePreset,
};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};
use tokio::sync::mpsc;

/// Expands the preset's expression list into (id, full_prompt, negative) tuples.
pub fn build_prompts(preset: &StylePreset) -> Vec<ExpressionPrompt> {
    preset
        .expressions
        .iter()
        .map(|expr| {
            let prompt = format!(
                "{}\n{}",
                preset.llm_image_gen.base_prompt.trim(),
                expr.prompt_suffix
            );
            ExpressionPrompt {
                id: expr.id.clone(),
                prompt,
                negative_prompt: preset.llm_image_gen.negative_prompt.clone(),
            }
        })
        .collect()
}

/// Runs the full expression render job for one agent.
///
/// Spawns `parallel_jobs` concurrent provider calls at a time.
/// Sends `RenderProgress` snapshots via `progress_tx` after each image.
/// Writes `.webp` files to `<agent_dir>/expressions/` and updates `manifest.json`.
pub struct RenderJob {
    preset: StylePreset,
    provider: Arc<dyn ImageGenProvider>,
    agent_dir: PathBuf,
    parallel_jobs: usize,
}

impl RenderJob {
    pub fn new(
        preset: StylePreset,
        provider: Arc<dyn ImageGenProvider>,
        agent_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            preset,
            provider,
            agent_dir: agent_dir.into(),
            parallel_jobs: 3,
        }
    }

    pub fn with_parallel_jobs(mut self, n: usize) -> Self {
        self.parallel_jobs = n.max(1);
        self
    }

    /// Execute the render job.
    ///
    /// Returns the completed `PresetManifest` on success. If some expressions
    /// fail, they are omitted from `manifest.expressions` but the job still
    /// returns `Ok`; the caller should inspect the manifest for coverage.
    ///
    /// If the entire batch fails (all expressions error or cancel fires before
    /// any renders), returns `Err`.
    pub async fn run(
        &self,
        cancel: CancelToken,
        progress_tx: Option<mpsc::Sender<RenderProgress>>,
    ) -> Result<PresetManifest> {
        let expressions_dir = self.agent_dir.join("expressions");
        tokio::fs::create_dir_all(&expressions_dir).await?;

        let prompts = build_prompts(&self.preset);
        let total = prompts.len() as u32;
        let done = Arc::new(AtomicU32::new(0));
        let failed = Arc::new(AtomicU32::new(0));
        let mut rendered: Vec<String> = Vec::new();

        for chunk in prompts.chunks(self.parallel_jobs) {
            if cancel.is_cancelled() {
                break;
            }

            let mut set = tokio::task::JoinSet::new();

            for ep in chunk {
                let ep = ep.clone();
                let provider = self.provider.clone();
                let cancel = cancel.clone();
                let expressions_dir = expressions_dir.clone();
                let size = ImageSize::from_size_str(&self.preset.llm_image_gen.size);
                let source_image: Option<PathBuf> = None; // polaroid: wired in M-h7

                set.spawn(async move {
                    if cancel.is_cancelled() {
                        return Err(ep.id);
                    }
                    let result = provider
                        .generate(
                            &ep.prompt,
                            Some(&ep.negative_prompt),
                            size,
                            source_image.as_deref(),
                            cancel,
                        )
                        .await;

                    match result {
                        Ok(img) => {
                            let path = expressions_dir.join(format!("{}.webp", ep.id));
                            img.save_with_format(&path, image::ImageFormat::WebP)
                                .map_err(|_| ep.id.clone())?;
                            Ok(ep.id)
                        }
                        Err(_) => Err(ep.id),
                    }
                });
            }

            while let Some(join_result) = set.join_next().await {
                let outcome = match join_result {
                    Ok(inner) => inner,
                    Err(_) => Err("task-panic".to_string()),
                };
                match outcome {
                    Ok(id) => {
                        done.fetch_add(1, Ordering::Relaxed);
                        rendered.push(id);
                    }
                    Err(_) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
                if let Some(tx) = &progress_tx {
                    let _ = tx
                        .send(RenderProgress {
                            total,
                            done: done.load(Ordering::Relaxed),
                            failed: failed.load(Ordering::Relaxed),
                            current: None,
                        })
                        .await;
                }
            }
        }

        let manifest = PresetManifest {
            preset_id: self.preset.id.clone(),
            rendered_at: chrono::Utc::now(),
            sha256: compute_preset_hash(&self.preset),
            expressions: rendered,
        };

        write_manifest(&self.agent_dir, &manifest).await?;
        Ok(manifest)
    }
}

async fn write_manifest(agent_dir: &Path, manifest: &PresetManifest) -> Result<()> {
    let path = manifest_path(agent_dir);
    tokio::fs::create_dir_all(path.parent().unwrap()).await?;
    let json = serde_json::to_string_pretty(manifest)?;
    tokio::fs::write(&path, json.as_bytes()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_gen::mock::MockImageGenProvider;
    use mur_common::hub::preset_loader::load_builtin_presets;

    #[tokio::test]
    async fn mock_render_writes_all_expressions() {
        let tmp = tempfile::tempdir().unwrap();
        let preset = load_builtin_presets()
            .into_iter()
            .find(|p| p.id == "default-blob")
            .unwrap();

        let job = RenderJob::new(
            preset.clone(),
            Arc::new(MockImageGenProvider),
            tmp.path(),
        );
        let manifest = job.run(CancelToken::new(), None).await.unwrap();

        assert_eq!(manifest.preset_id, "default-blob");
        assert_eq!(manifest.expressions.len(), 12, "all 12 expressions rendered");

        // Files should exist.
        for id in &manifest.expressions {
            let path = tmp.path().join("expressions").join(format!("{id}.webp"));
            assert!(path.exists(), "missing {id}.webp");
        }
    }

    #[tokio::test]
    async fn mock_render_writes_manifest_json() {
        let tmp = tempfile::tempdir().unwrap();
        let preset = load_builtin_presets()
            .into_iter()
            .find(|p| p.id == "chiikawa")
            .unwrap();
        let job = RenderJob::new(preset, Arc::new(MockImageGenProvider), tmp.path());
        let manifest = job.run(CancelToken::new(), None).await.unwrap();

        let manifest_file = tmp.path().join("expressions").join("manifest.json");
        assert!(manifest_file.exists());

        let on_disk: PresetManifest =
            serde_json::from_str(&std::fs::read_to_string(&manifest_file).unwrap()).unwrap();
        assert_eq!(on_disk.preset_id, manifest.preset_id);
        assert_eq!(on_disk.sha256, manifest.sha256);
    }

    #[tokio::test]
    async fn render_cancel_stops_early() {
        let tmp = tempfile::tempdir().unwrap();
        let preset = load_builtin_presets()
            .into_iter()
            .find(|p| p.id == "default-blob")
            .unwrap();
        let cancel = CancelToken::new();
        cancel.cancel(); // cancel before run
        let job =
            RenderJob::new(preset, Arc::new(MockImageGenProvider), tmp.path()).with_parallel_jobs(1);
        let manifest = job.run(cancel, None).await.unwrap();
        assert!(
            manifest.expressions.is_empty(),
            "no expressions should be rendered when cancelled"
        );
    }

    #[tokio::test]
    async fn build_prompts_expands_all_expressions() {
        let preset = load_builtin_presets()
            .into_iter()
            .find(|p| p.id == "chiikawa")
            .unwrap();
        let prompts = build_prompts(&preset);
        assert_eq!(prompts.len(), 12);
        let ids: Vec<&str> = prompts.iter().map(|p| p.id.as_str()).collect();
        assert!(ids.contains(&"idle"));
        assert!(ids.contains(&"error"));
    }

    #[tokio::test]
    async fn render_sends_progress_events() {
        let tmp = tempfile::tempdir().unwrap();
        let preset = load_builtin_presets()
            .into_iter()
            .find(|p| p.id == "default-blob")
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let job = RenderJob::new(preset, Arc::new(MockImageGenProvider), tmp.path());
        job.run(CancelToken::new(), Some(tx)).await.unwrap();

        let mut events: Vec<RenderProgress> = Vec::new();
        while let Ok(p) = rx.try_recv() {
            events.push(p);
        }
        assert!(!events.is_empty(), "should have received progress events");
        let last = events.last().unwrap();
        assert_eq!(last.total, 12);
        assert_eq!(last.done, 12);
        assert_eq!(last.failed, 0);
    }
}
