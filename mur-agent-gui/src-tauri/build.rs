fn main() {
    // M1.2.1: dev-time placeholder verifying key for the voice manifest
    // signature pipeline. Real release builds MUST override via the
    // MUR_VOICE_PUBKEY env var (e.g. set in CI before tauri build).
    // This dev key is non-functional (all-zeros pubkey rejects every
    // signature) — sufficient to make compile-time `env!()` succeed
    // without bypassing verification at runtime.
    if std::env::var("MUR_VOICE_PUBKEY").is_err() {
        // 32-byte zero pubkey, base58btc multibase-encoded with leading 'z'.
        println!("cargo:rustc-env=MUR_VOICE_PUBKEY=z11111111111111111111111111111111");
    }
    println!("cargo:rerun-if-env-changed=MUR_VOICE_PUBKEY");

    tauri_build::build()
}
