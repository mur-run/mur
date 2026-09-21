fn main() {
    // If MUR_WEB_DIST is not set, use the fallback placeholder
    if std::env::var("MUR_WEB_DIST").is_err() {
        // Read CARGO_MANIFEST_DIR at *run* time, not compile time: `env!` would
        // bake in the path of whatever checkout built this script, which goes
        // stale the moment that worktree is removed.
        let manifest_dir =
            std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo");
        let fallback = std::path::Path::new(&manifest_dir).join("web-fallback");
        println!("cargo:rustc-env=MUR_WEB_DIST={}", fallback.display());
    }
    // Re-run if web dist changes
    println!("cargo:rerun-if-env-changed=MUR_WEB_DIST");
}
