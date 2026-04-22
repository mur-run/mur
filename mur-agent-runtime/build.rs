//! Build script — bundles an agent home directory into the binary when the
//! `embedded-agent` feature is enabled and `MUR_EXPORT_AGENT_DIR` is set.
//!
//! Even without either, we still emit `MUR_EMBEDDED_AGENT_PATH` pointing at a
//! zero-byte placeholder so `include_bytes!(env!(...))` in `bin_embed.rs`
//! always has a valid target (though it is cfg-gated away in default builds).

use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=MUR_EXPORT_AGENT_DIR");

    let has_feature = env::var_os("CARGO_FEATURE_EMBEDDED_AGENT").is_some();
    let src_dir = env::var_os("MUR_EXPORT_AGENT_DIR").map(PathBuf::from);
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR must be set"));
    let out_path = out_dir.join("embedded_agent.tar.gz");

    if has_feature && let Some(src) = src_dir.as_ref() {
        if !src.exists() {
            panic!(
                "embedded-agent feature enabled but MUR_EXPORT_AGENT_DIR = {} does not exist",
                src.display()
            );
        }
        build_tar(src, &out_path).expect("build embedded_agent.tar.gz");
        println!("cargo:rerun-if-changed={}", src.display());
    } else {
        fs::write(&out_path, b"").expect("write placeholder embedded_agent.tar.gz");
    }

    println!(
        "cargo:rustc-env=MUR_EMBEDDED_AGENT_PATH={}",
        out_path.display()
    );
}

fn build_tar(src: &Path, out_path: &Path) -> std::io::Result<()> {
    let file = File::create(out_path)?;
    let gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    tar.append_dir_all(".", src)?;
    tar.into_inner()?.finish()?;
    Ok(())
}
