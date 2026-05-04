//! Library crate facing — Tauri's macros generate FFI bindings into
//! the cdylib/staticlib targets. The bin (`main.rs`) reuses the same
//! command + theme modules.

pub mod bootstrap;
pub mod commands;
pub mod companion_bridge;
pub mod multimodal;
pub mod send;
pub mod sidecar;
pub mod test_harness;
pub mod theme;
pub mod voice;

/// Process-wide env-var mutex shared by every `#[cfg(test)]` block
/// that mutates `std::env`. Tests across modules (`bootstrap`,
/// `send::wiring`) all touch `MUR_HOME` / `MUR_AGENT_BIN_DIR` /
/// `MUR_GUI_AGENT_NAME`, and `cargo test --lib` runs them in
/// parallel — without a shared lock they race and one test's
/// `set_var` clobbers another's view mid-assertion. A single static
/// here keeps them serial without forcing `--test-threads=1` for
/// the whole CI job.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
