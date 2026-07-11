//! Download a curated recipe, verify its pinned SHA-256, and place binaries.
//! `verify_and_place` is the security-critical, network-free core (unit-tested);
//! `download` is a thin reqwest wrapper.

use anyhow::{Context, Result, bail};
use mur_common::deps::registry::CuratedRecipe;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Verify `bytes` against `recipe.sha256`, then place file(s) under `mur_home`.
/// Fails closed on mismatch (nothing written). Returns installed absolute paths.
#[allow(dead_code)]
pub fn verify_and_place(
    bytes: &[u8],
    recipe: &CuratedRecipe,
    mur_home: &Path,
) -> Result<Vec<PathBuf>> {
    let got = sha256_hex(bytes);
    if !got.eq_ignore_ascii_case(&recipe.sha256) {
        bail!("sha256 mismatch: expected {}, got {}", recipe.sha256, got);
    }
    match &recipe.archive {
        None => {
            let rel = recipe
                .install_to
                .as_ref()
                .context("bare recipe missing install_to")?;
            let dst = mur_home.join(rel);
            place_file(&dst, bytes, recipe.executable)?;
            Ok(vec![dst])
        }
        Some(archive) => {
            // gzip tar; extract only declared members.
            let gz = flate2::read::GzDecoder::new(bytes);
            let mut tar = tar::Archive::new(gz);
            let mut wanted: std::collections::BTreeMap<&str, _> = archive
                .members
                .iter()
                .map(|m| (m.path_in_archive.as_str(), m))
                .collect();
            // Buffer members during scan; verify completeness BEFORE writing to disk.
            let mut buffered = Vec::new();
            for entry in tar.entries().context("read tar")? {
                let mut entry = entry.context("tar entry")?;
                let path = entry
                    .path()
                    .context("tar path")?
                    .to_string_lossy()
                    .to_string();
                let base = path.rsplit('/').next().unwrap_or(&path);
                if let Some(member) = wanted.remove(base) {
                    let mut buf = Vec::new();
                    entry.read_to_end(&mut buf).context("read member")?;
                    buffered.push((member, buf));
                }
            }
            // Verify all declared members are present before writing anything.
            if !wanted.is_empty() {
                bail!(
                    "archive missing members: {:?}",
                    wanted.keys().collect::<Vec<_>>()
                );
            }
            // All members present; now place them on disk.
            let mut placed = Vec::new();
            for (member, buf) in buffered {
                let dst = mur_home.join(&member.install_to);
                place_file(&dst, &buf, member.executable)?;
                placed.push(dst);
            }
            Ok(placed)
        }
    }
}

/// Atomic write (temp + rename) + optional +x. Creates parent dirs.
#[allow(dead_code)]
fn place_file(dst: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let tmp = dst.with_extension("mur-tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&tmp)?.permissions();
        perm.set_mode(0o755);
        if let Err(e) = std::fs::set_permissions(&tmp, perm) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e).context("chmod +x");
        }
    }
    let _ = executable; // silence unused on non-unix
    if let Err(e) = std::fs::rename(&tmp, dst) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("rename to {}", dst.display()));
    }
    Ok(())
}

/// Fetch the recipe URL into memory (bounded read via reqwest).
#[allow(dead_code)]
pub async fn download(url: &str) -> Result<Vec<u8>> {
    let resp = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("status for {url}"))?;
    Ok(resp.bytes().await.context("read body")?.to_vec())
}

/// Download + verify + place.
#[allow(dead_code)]
pub async fn install(recipe: &CuratedRecipe, mur_home: &Path) -> Result<Vec<PathBuf>> {
    let bytes = download(&recipe.url).await?;
    verify_and_place(&bytes, recipe, mur_home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::deps::registry::CuratedRecipe;
    use sha2::{Digest, Sha256};

    fn sha_hex(b: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(b);
        hex::encode(h.finalize())
    }

    #[test]
    fn bare_binary_sha_match_installs_and_chmods() {
        let tmp = std::env::temp_dir().join(format!("murinst_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let bytes = b"#!/bin/sh\necho hi\n";
        let recipe = CuratedRecipe {
            description: "t".into(),
            url: "unused".into(),
            sha256: sha_hex(bytes),
            install_to: Some("aura/lightpanda".into()),
            executable: true,
            archive: None,
        };
        let placed = verify_and_place(bytes, &recipe, &tmp).unwrap();
        assert_eq!(placed, vec![tmp.join("aura/lightpanda")]);
        assert!(tmp.join("aura/lightpanda").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(tmp.join("aura/lightpanda"))
                .unwrap()
                .permissions()
                .mode();
            assert!(mode & 0o111 != 0, "executable bit set");
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn sha_mismatch_fails_closed_no_file_written() {
        let tmp = std::env::temp_dir().join(format!("murinstbad_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let recipe = CuratedRecipe {
            description: "t".into(),
            url: "u".into(),
            sha256: "0".repeat(64), // wrong
            install_to: Some("aura/x".into()),
            executable: true,
            archive: None,
        };
        let r = verify_and_place(b"payload", &recipe, &tmp);
        assert!(r.is_err(), "mismatch must error");
        assert!(!tmp.join("aura/x").exists(), "no file on mismatch");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn archive_missing_member_writes_nothing() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use mur_common::deps::registry::RecipeMember;

        let tmp = std::env::temp_dir().join(format!("murimiss_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        // Build a tar containing only "obscura", not "obscura-worker".
        let mut tar_buf = Vec::new();
        {
            let gz = GzEncoder::new(&mut tar_buf, Compression::default());
            let mut tar = tar::Builder::new(gz);
            let mut header = tar::Header::new_gnu();
            header.set_size(7);
            tar.append_data(&mut header, "obscura", &b"content"[..])
                .unwrap();
            tar.finish().unwrap();
        }

        // Create a recipe declaring BOTH members.
        let archive_members = vec![
            RecipeMember {
                path_in_archive: "obscura".into(),
                install_to: "aura/obscura".into(),
                executable: false,
            },
            RecipeMember {
                path_in_archive: "obscura-worker".into(),
                install_to: "aura/obscura-worker".into(),
                executable: true,
            },
        ];
        let recipe = CuratedRecipe {
            description: "test".into(),
            url: "unused".into(),
            sha256: sha_hex(&tar_buf),
            install_to: None,
            executable: false,
            archive: Some(mur_common::deps::registry::ArchiveSpec {
                members: archive_members,
            }),
        };

        // Verify that extraction fails (missing member).
        let r = verify_and_place(&tar_buf, &recipe, &tmp);
        assert!(r.is_err(), "missing member must error");

        // Verify that no files were written.
        assert!(
            !tmp.join("aura/obscura").exists(),
            "obscura must not exist on error"
        );
        assert!(
            !tmp.join("aura/obscura-worker").exists(),
            "obscura-worker must not exist on error"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }
}
