//! `mur code-nav setup` (plan 2026-10-03 code-nav, Phase 3; decision P3-D1:
//! a standalone command shaped like `mur browser setup`).
//!
//! The pure planner (3.1), both installers (3.2, 3.3), the serena config
//! generator (3.4), the profile entry (3.5), and the consent flow that
//! applies them (3.6, [`setup::run`]).

pub mod ast_grep_install;
pub mod consent;
pub mod plan;
pub mod serena_config;
pub mod serena_entry;
pub mod serena_install;
pub mod serena_project;
pub mod setup;
