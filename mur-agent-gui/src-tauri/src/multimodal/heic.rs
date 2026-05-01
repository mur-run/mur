//! HEIC → PNG normalization.
//!
//! HEIC is the default iPhone format. Apple Photos exports them with
//! EXIF GPS coordinates and sometimes embedded thumbnails — both bad
//! for our threat model. We always re-encode to PNG so the metadata
//! is stripped by construction (decode → raw RGBA → re-encode strips
//! every container-level extension).
//!
//! v1 implementation: shells out to macOS `/usr/bin/sips` for the HEIC
//! → PNG decode, then pipes the result through `image-rs` to strip
//! ancillary PNG chunks. This avoids a hard dependency on the
//! `libheif` system library, which is not universally installed on
//! dev machines and is painful in CI. The tradeoff is HEIC support
//! is currently macOS-only — Linux and Windows return a clear error
//! directing the user to install libheif-dev and re-enable the
//! libheif-rs dep (see Cargo.toml's `# ─── multimodal decoder ───`
//! comment block).
//!
//! Why the second pass through `image-rs`: empirically, sips
//! preserves `eXIf` and `iTXt` (XMP) chunks in its PNG output, so a
//! single sips conversion is *not* sufficient to strip metadata.
//! Re-encoding through `image-rs` reads only the IDAT pixel data and
//! emits a PNG with only critical chunks (IHDR/IDAT/IEND), which is
//! the actual metadata-strip we promise.
//!
//! Spec: roadmap §4.3 step 3.

// `Context` is only used by the macOS-only sips path; on other
// platforms the function is a one-line `bail!` so the import would be
// dead.
#[cfg(target_os = "macos")]
use anyhow::Context;
use anyhow::{Result, bail};

/// Verify the input has an ISO-BMFF `ftyp` box with a HEIC-family brand.
/// Without this guard, `sips` would happily treat any image as input
/// and "convert" it to PNG, which is not what callers want.
#[cfg(target_os = "macos")]
fn has_heic_magic(bytes: &[u8]) -> bool {
    if bytes.len() < 12 {
        return false;
    }
    if &bytes[4..8] != b"ftyp" {
        return false;
    }
    matches!(
        &bytes[8..12],
        b"heic"
            | b"heix"
            | b"hevc"
            | b"hevx"
            | b"mif1"
            | b"msf1"
            | b"heim"
            | b"heis"
            | b"hevm"
            | b"hevs"
    )
}

/// Decode HEIC bytes to an in-memory PNG. EXIF / XMP / GPS / thumbnails
/// are stripped because the conversion goes through a re-encode cycle.
#[cfg(target_os = "macos")]
pub fn heic_to_png(heic: &[u8]) -> Result<Vec<u8>> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    if !has_heic_magic(heic) {
        bail!("input is not a HEIC/HEIF file (missing ftyp box with HEIC brand)");
    }

    // Write to a tempfile because `sips` reads from a path, not stdin.
    let in_tmp = tempfile::Builder::new()
        .suffix(".heic")
        .tempfile()
        .context("create heic tempfile")?;
    let out_tmp = tempfile::Builder::new()
        .suffix(".png")
        .tempfile()
        .context("create png tempfile")?;

    {
        let mut f = in_tmp.as_file();
        f.write_all(heic).context("write heic tempfile")?;
        f.flush().ok();
    }

    let status = Command::new("/usr/bin/sips")
        .args([
            "-s",
            "format",
            "png",
            in_tmp.path().to_str().context("heic path utf8")?,
            "--out",
            out_tmp.path().to_str().context("png path utf8")?,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("spawn sips")?;

    if !status.success() {
        bail!(
            "sips: HEIC → PNG conversion failed (exit {:?})",
            status.code()
        );
    }

    let sips_png = std::fs::read(out_tmp.path()).context("read converted png")?;
    if !sips_png.starts_with(&[0x89, 0x50, 0x4e, 0x47]) {
        bail!("sips output is not a valid PNG (header mismatch)");
    }

    // Second pass: decode via image-rs and re-encode, dropping every
    // ancillary chunk (eXIf / iTXt / tEXt / zTXt / iCCP / pHYs / …).
    // This is what actually delivers the "metadata stripped" promise.
    let img = image::load_from_memory_with_format(&sips_png, image::ImageFormat::Png)
        .context("decode sips PNG via image-rs")?;
    let mut clean = Vec::with_capacity(sips_png.len());
    img.write_to(
        &mut std::io::Cursor::new(&mut clean),
        image::ImageFormat::Png,
    )
    .context("re-encode PNG to strip metadata")?;
    if !clean.starts_with(&[0x89, 0x50, 0x4e, 0x47]) {
        bail!("re-encoded PNG header mismatch");
    }
    Ok(clean)
}

/// Non-macOS: HEIC support requires libheif-dev + the libheif-rs crate
/// (see Cargo.toml). Re-enable both when this is needed.
#[cfg(not(target_os = "macos"))]
pub fn heic_to_png(_heic: &[u8]) -> Result<Vec<u8>> {
    bail!(
        "HEIC decoding not supported on this platform yet — install \
         libheif-dev (or equivalent) and re-enable the libheif-rs dep \
         in mur-agent-gui/src-tauri/Cargo.toml"
    )
}
