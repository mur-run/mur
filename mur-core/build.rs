fn main() {
    // If MUR_WEB_DIST is not set, use the fallback placeholder
    if std::env::var("MUR_WEB_DIST").is_err() {
        let fallback = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web-fallback");
        println!("cargo:rustc-env=MUR_WEB_DIST={}", fallback.display());
    }
    // Re-run if web dist changes
    println!("cargo:rerun-if-env-changed=MUR_WEB_DIST");
}
