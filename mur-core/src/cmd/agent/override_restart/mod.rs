//! `mur agent start|restart <name>` with a startup-override flag: one
//! orchestrator shared by the CLI and (later) the Hub. It writes the
//! one-shot override file, launches the boot, and reports what that boot did
//! with it.
//!
//! Specs: `2026-10-25-startup-permission-overrides-design.md` (§When the file
//! is written, §Acknowledgement) and `2026-10-25-startup-overrides-hub-design.md`
//! (§Orchestrator).

// The orchestrator that calls these lands in the same PR, task by task.
#[allow(dead_code)]
mod writer;
