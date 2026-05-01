//! `mur agent companion card` — character card I/O (D4).
//!
//! Submodules land progressively: M4.3 (png) → M4.4 (cai) → M4.5
//! (inbox) → M4.6 (accept) → M4.7 (CLI dispatch + export).

pub mod accept;
pub mod cai;
pub mod export;
pub mod import;
pub mod list;
pub mod png;
