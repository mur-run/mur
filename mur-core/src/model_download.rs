//! Download a HuggingFace model repo (a folder of files) into a local dir,
//! reporting aggregate byte progress. Reuses the temp-file + atomic-rename
//! idiom and writes a `.complete` sentinel only on full success, so a partial
//! download never looks cached. Used by the Hub first-run flow (and reusable
//! by the CLI later).

use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::Path;

/// HuggingFace site root. Named (not inlined) per the no-hardcoded-values rule.
const HF_BASE: &str = "https://huggingface.co";
/// User-Agent sent with HF requests.
const HF_USER_AGENT: &str = "mur-hub";

/// One file in a HF repo listing.
#[derive(Debug, Clone, PartialEq)]
pub struct HfFile {
    pub name: String,
    pub size: u64,
}

/// Parse the `siblings[]` of a HF `/api/models/<repo>?blobs=true` response into
/// a flat file list. `size` defaults to 0 when the API omits it (progress then
/// reports an indeterminate total).
pub fn parse_hf_files(meta: &serde_json::Value) -> Vec<HfFile> {
    meta.get("siblings")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let name = s.get("rfilename")?.as_str()?.to_string();
                    let size = s.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
                    Some(HfFile { name, size })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Download every file of `repo` into `dest`, calling `on_progress(done, total)`
/// (bytes) as it streams. Returns immediately if `dest/.complete` already exists.
pub async fn download_hf_model(
    repo: &str,
    dest: &Path,
    on_progress: impl Fn(u64, u64),
) -> Result<()> {
    let marker = mur_common::local_llm::model_complete_marker(dest);
    if marker.is_file() {
        return Ok(()); // already downloaded — the cache from the design's Q4.
    }
    std::fs::create_dir_all(dest).with_context(|| format!("create {}", dest.display()))?;

    let client = reqwest::Client::builder()
        .user_agent(HF_USER_AGENT)
        .build()
        .context("build http client")?;

    let api = format!("{HF_BASE}/api/models/{repo}?blobs=true");
    let meta: serde_json::Value = client
        .get(&api)
        .send()
        .await
        .with_context(|| format!("list {repo}"))?
        .error_for_status()
        .with_context(|| format!("list {repo}"))?
        .json()
        .await
        .context("parse HF model listing")?;

    let files = parse_hf_files(&meta);
    if files.is_empty() {
        bail!("no files listed for HF repo '{repo}'");
    }
    let total: u64 = files.iter().map(|f| f.size).sum();
    let mut done: u64 = 0;

    for f in &files {
        let url = format!("{HF_BASE}/{repo}/resolve/main/{}", f.name);
        let final_path = dest.join(&f.name);
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Temp file lives in `dest` (same filesystem → atomic rename). Sanitize
        // any '/' so a nested rfilename can't escape the temp name.
        let tmp = dest.join(format!("{}.part", f.name.replace('/', "__")));

        let mut resp = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("download {}", f.name))?
            .error_for_status()
            .with_context(|| format!("download {}", f.name))?;

        let mut file =
            std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        while let Some(chunk) = resp
            .chunk()
            .await
            .with_context(|| format!("stream {}", f.name))?
        {
            file.write_all(&chunk)?;
            done += chunk.len() as u64;
            on_progress(done, total);
        }
        file.flush()?;
        drop(file);
        std::fs::rename(&tmp, &final_path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), final_path.display()))?;
    }

    // Marker last: a partial download is never mistaken for complete.
    std::fs::write(&marker, b"ok").with_context(|| format!("write {}", marker.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_siblings_with_and_without_size() {
        let meta = serde_json::json!({
            "siblings": [
                {"rfilename": "model.safetensors", "size": 1000u64},
                {"rfilename": "config.json"},
            ]
        });
        let files = parse_hf_files(&meta);
        assert_eq!(files.len(), 2);
        assert_eq!(
            files[0],
            HfFile {
                name: "model.safetensors".into(),
                size: 1000
            }
        );
        assert_eq!(
            files[1],
            HfFile {
                name: "config.json".into(),
                size: 0
            }
        );
    }

    #[test]
    fn parses_missing_siblings_as_empty() {
        assert!(parse_hf_files(&serde_json::json!({})).is_empty());
    }

    #[tokio::test]
    async fn complete_marker_short_circuits_without_network() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path();
        std::fs::write(mur_common::local_llm::model_complete_marker(dest), b"ok").unwrap();
        // Bogus repo: must NOT be contacted because the marker exists.
        download_hf_model("does/not-exist", dest, |_, _| {})
            .await
            .unwrap();
    }
}
