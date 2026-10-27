//! One-shot startup permission overrides: the on-disk schema shared by the
//! writers (`mur agent start|restart`, the Hub) and the runtime that applies it.
//!
//! Spec: `2026-10-25-startup-permission-overrides-design.md`
//! (§`IssuedBy` / `ConfirmedBy`, §Override file, §Runtime boot sequence).

use serde::{Deserialize, Serialize};

pub mod timing;
pub use timing::{
    OVERRIDE_ACK_WAIT_SECS, OVERRIDE_AGE_MARGIN_SECS, RESPAWN_WAIT_SECS, RESTART_KILL_GRACE_SECS,
    SLOW_START_WAIT_SECS, override_max_age,
};

/// Which surface wrote the override file.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum IssuedBy {
    #[serde(rename = "cli")]
    Cli,
    #[serde(rename = "hub")]
    Hub,
}

/// How the human confirmed the override. Only some pairings with
/// [`IssuedBy`] are legal; see [`is_valid_pair`].
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfirmedBy {
    #[serde(rename = "tty")]
    Tty,
    #[serde(rename = "flag:--confirm")]
    FlagConfirm,
    #[serde(rename = "hub:native-dialog+typed-name")]
    HubNativeDialogTypedName,
}

/// The pairing table, defined once. Writers refuse to write any other pair;
/// the runtime refuses to apply one.
pub fn is_valid_pair(issued: IssuedBy, confirmed: ConfirmedBy) -> bool {
    matches!(
        (issued, confirmed),
        (IssuedBy::Cli, ConfirmedBy::Tty)
            | (IssuedBy::Cli, ConfirmedBy::FlagConfirm)
            | (IssuedBy::Hub, ConfirmedBy::HubNativeDialogTypedName)
    )
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod timing_tests;
