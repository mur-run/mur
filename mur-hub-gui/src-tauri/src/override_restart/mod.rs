//! The Hub's startup-override restart (Hub spec §Orchestrator, §Launchers,
//! §Quit). The orchestrator itself lives in `mur_core`; this module only
//! supplies the Hub's job table, launcher, confirmation and commands.

pub mod jobs;
pub mod launcher;
