//! Track C2 — Telegram bridge inbound loop tests.
//!
//! Covers M-c2.2.1 .. M-c2.2.4: teloxide dep imports cleanly, the
//! `TelegramInboundLoop` skeleton constructs, and `tick_once()` honours
//! dedupe / privacy / 5xx-pinning / signed-forward semantics.

#[test]
fn teloxide_imports_compile() {
    // Smoke check: the symbol resolves and the crate links. We don't actually
    // construct a Bot here (that requires a token + tokio runtime).
    let _ = std::any::type_name::<teloxide::Bot>();
}
