fn main() {
    // The directory RustEmbed (`#[folder = "$MUR_WEB_DIST"]` in server/mod.rs)
    // embeds: the caller's MUR_WEB_DIST, else the in-tree placeholder.
    let web_dist = match std::env::var("MUR_WEB_DIST") {
        Ok(dir) => std::path::PathBuf::from(dir),
        Err(_) => {
            // Read CARGO_MANIFEST_DIR at *run* time, not compile time: `env!`
            // would bake in the path of whatever checkout built this script,
            // which goes stale the moment that worktree is removed.
            let manifest_dir =
                std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo");
            let fallback = std::path::Path::new(&manifest_dir).join("web-fallback");
            println!("cargo:rustc-env=MUR_WEB_DIST={}", fallback.display());
            fallback
        }
    };
    // Re-run when the variable changes, and when the directory it resolves to
    // changes or disappears. Without the second line a cached run keeps
    // pointing RustEmbed at a removed worktree's path until build.rs is touched.
    println!("cargo:rerun-if-env-changed=MUR_WEB_DIST");
    println!("cargo:rerun-if-changed={}", web_dist.display());
}
