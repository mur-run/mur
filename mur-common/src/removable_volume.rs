//! Shared, logic-free constants for the macOS removable-volume (external
//! disk) Full-Disk-Access failure mode (issue #1).
//!
//! This module deliberately holds **only** the canonical guidance string —
//! no logic — so it can live in `mur-common` and be shared verbatim by both
//! the agent runtime's file/bash tools and `mur agent doctor`. The judgement
//! helpers (e.g. `is_removable_volume_eperm`) stay in the runtime's
//! `fs_policy`; only the wording is centralized here so it never drifts.

/// The one canonical, structured guidance shown whenever a `/Volumes/*`
/// (removable / external volume) path is blocked by macOS with EPERM
/// (`os error 1`, "Operation not permitted"). Shared by the file/bash tools
/// and `mur agent doctor` so the wording never drifts (issue #1). Kept as a
/// single `const` — do NOT duplicate this string anywhere.
pub const REMOVABLE_VOLUME_EPERM_HINT: &str = "macOS 檔案權限擋住了外接磁碟。請到 系統設定→隱私權與安全性→完全取用磁碟 開啟 MUR Hub(或啟動此 agent 的 app),然後重啟 agent";
