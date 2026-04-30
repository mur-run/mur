//! Voice download client. Streams bytes from a signed CDN, computes
//! SHA-256 incrementally, verifies against the manifest, and stages
//! each asset atomically (temp file + rename).
//!
//! Used by both `voice_download` (per-voice packs) and `voice_stt_download`
//! (the whisper STT model). The two share schema + verification +
//! atomicity guarantees; only the manifest deserialisation differs.

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use super::manifest::{
    AssetBundle, AssetEntry, SttModelManifest, VoiceManifest, verify_and_parse, verify_signature,
};

const CONNECT_TIMEOUT_S: u64 = 10;
const TOTAL_TIMEOUT_S: u64 = 600;

/// Override for tests / staging via `MUR_VOICE_CDN_BASE`.
fn cdn_base() -> String {
    std::env::var("MUR_VOICE_CDN_BASE").unwrap_or_else(|_| "https://voices.mur.run".to_string())
}

/// Progress events streamed back to the GUI for the progress UI.
/// Frontend listens on `voice://download-progress` (voices) or
/// `voice://stt-download-progress` (STT model).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum DownloadProgress {
    ManifestFetched,
    ManifestVerified,
    AssetStarted {
        name: String,
        size_bytes: u64,
    },
    AssetProgress {
        name: String,
        downloaded_bytes: u64,
        total_bytes: u64,
    },
    AssetComplete {
        name: String,
    },
    Done,
}

#[derive(Debug, Clone)]
pub struct DownloadHandle {
    pub bundle_id: String,
    pub install_dir: PathBuf,
}

/// Download a voice pack. The downloaded `manifest.json` + `manifest.json.sig`
/// are persisted alongside the assets so later launches can re-verify
/// integrity without re-fetching the network.
pub async fn download_voice(
    voice_id: &str,
    install_dir: PathBuf,
    progress: tokio::sync::mpsc::Sender<DownloadProgress>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<DownloadHandle> {
    let client = build_client()?;

    let manifest_url = format!("{}/{voice_id}/manifest.json", cdn_base());
    let sig_url = format!("{}/{voice_id}/manifest.json.sig", cdn_base());

    let manifest_bytes = client
        .get(&manifest_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let _ = progress.send(DownloadProgress::ManifestFetched).await;

    let sig_bytes = client
        .get(&sig_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    let bundle = verify_and_parse(&manifest_bytes, &sig_bytes)
        .context("voice manifest verification failed; refusing to install")?;
    let _ = progress.send(DownloadProgress::ManifestVerified).await;

    let manifest: VoiceManifest = match bundle {
        AssetBundle::Voice(v) => v,
        AssetBundle::SttModel(_) => bail!("expected `kind: voice` manifest, got STT model"),
    };
    if manifest.voice_id != voice_id {
        bail!(
            "manifest voice_id `{}` does not match request `{voice_id}`",
            manifest.voice_id
        );
    }

    fs::create_dir_all(&install_dir).await?;
    for asset in &manifest.assets {
        if cancel.is_cancelled() {
            bail!("download cancelled");
        }
        download_one_asset(&client, asset, &install_dir, &progress, &cancel)
            .await
            .with_context(|| format!("downloading asset {}", asset.name))?;
    }

    fs::write(install_dir.join("manifest.json"), &manifest_bytes).await?;
    fs::write(install_dir.join("manifest.json.sig"), &sig_bytes).await?;

    let _ = progress.send(DownloadProgress::Done).await;
    Ok(DownloadHandle {
        bundle_id: voice_id.into(),
        install_dir,
    })
}

/// Download an STT model bundle (whisper-large-v3-turbo-q5_1 in v1).
/// Mirrors `download_voice` but expects `kind: stt-model`.
pub async fn download_stt_model(
    model_id: &str,
    install_dir: PathBuf,
    progress: tokio::sync::mpsc::Sender<DownloadProgress>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<DownloadHandle> {
    let client = build_client()?;

    let manifest_url = format!("{}/_stt/{model_id}/manifest.json", cdn_base());
    let sig_url = format!("{}/_stt/{model_id}/manifest.json.sig", cdn_base());

    let manifest_bytes = client
        .get(&manifest_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let _ = progress.send(DownloadProgress::ManifestFetched).await;

    let sig_bytes = client
        .get(&sig_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    verify_signature(&manifest_bytes, &sig_bytes)
        .context("STT manifest signature verification failed; refusing to install")?;
    let _ = progress.send(DownloadProgress::ManifestVerified).await;

    let manifest: SttModelManifest = serde_json::from_slice(&manifest_bytes)
        .context("STT manifest is not valid SttModelManifest JSON")?;
    if manifest.model_id != model_id {
        bail!(
            "manifest model_id `{}` does not match request `{model_id}`",
            manifest.model_id
        );
    }

    fs::create_dir_all(&install_dir).await?;
    for asset in &manifest.assets {
        if cancel.is_cancelled() {
            bail!("download cancelled");
        }
        download_one_asset(&client, asset, &install_dir, &progress, &cancel)
            .await
            .with_context(|| format!("downloading asset {}", asset.name))?;
    }

    fs::write(install_dir.join("manifest.json"), &manifest_bytes).await?;
    fs::write(install_dir.join("manifest.json.sig"), &sig_bytes).await?;

    let _ = progress.send(DownloadProgress::Done).await;
    Ok(DownloadHandle {
        bundle_id: model_id.into(),
        install_dir,
    })
}

fn build_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(CONNECT_TIMEOUT_S))
        .timeout(std::time::Duration::from_secs(TOTAL_TIMEOUT_S))
        .build()?)
}

async fn download_one_asset(
    client: &reqwest::Client,
    asset: &AssetEntry,
    install_dir: &Path,
    progress: &tokio::sync::mpsc::Sender<DownloadProgress>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    let _ = progress
        .send(DownloadProgress::AssetStarted {
            name: asset.name.clone(),
            size_bytes: asset.size_bytes,
        })
        .await;

    let resp = client.get(&asset.url).send().await?.error_for_status()?;
    let mut stream = resp.bytes_stream();

    let tmp = install_dir.join(format!("{}.partial", asset.name));
    let final_path = install_dir.join(&asset.name);
    let mut file = fs::File::create(&tmp).await?;
    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0;
    let mut last_progress = std::time::Instant::now();

    while let Some(chunk) = stream.next().await {
        if cancel.is_cancelled() {
            drop(file);
            let _ = fs::remove_file(&tmp).await;
            bail!("download cancelled mid-asset");
        }
        let bytes = chunk?;
        hasher.update(&bytes);
        file.write_all(&bytes).await?;
        downloaded += bytes.len() as u64;
        if last_progress.elapsed() > std::time::Duration::from_millis(150) {
            let _ = progress
                .send(DownloadProgress::AssetProgress {
                    name: asset.name.clone(),
                    downloaded_bytes: downloaded,
                    total_bytes: asset.size_bytes,
                })
                .await;
            last_progress = std::time::Instant::now();
        }
    }
    file.flush().await?;
    file.sync_all().await?;
    drop(file);

    if downloaded != asset.size_bytes {
        let _ = fs::remove_file(&tmp).await;
        bail!(
            "size mismatch on {}: got {}, expected {}",
            asset.name,
            downloaded,
            asset.size_bytes
        );
    }
    let actual = hex::encode(hasher.finalize());
    if actual != asset.sha256_hex {
        let _ = fs::remove_file(&tmp).await;
        bail!(
            "sha256 mismatch on {}: got {actual}, expected {}",
            asset.name,
            asset.sha256_hex
        );
    }

    fs::rename(&tmp, &final_path).await?;
    let _ = progress
        .send(DownloadProgress::AssetComplete {
            name: asset.name.clone(),
        })
        .await;
    Ok(())
}
