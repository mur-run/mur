//! Library crate facing — Tauri's macros generate FFI bindings into
//! the cdylib/staticlib targets. The bin (`main.rs`) reuses the same
//! command + theme modules.

pub mod bootstrap;
pub mod commands;
pub mod sidecar;
pub mod theme;
