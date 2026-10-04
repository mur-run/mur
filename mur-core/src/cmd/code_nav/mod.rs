//! `mur code-nav setup` (plan 2026-10-03 code-nav, Phase 3; decision P3-D1:
//! a standalone command shaped like `mur browser setup`).
//!
//! Done: the pure planner (3.1), both installers (3.2, 3.3) and the serena
//! config generator (3.4). The profile entry and the consent flow (3.5,
//! 3.6) build on [`plan::Plan`].

pub mod ast_grep_install;
pub mod plan;
pub mod serena_config;
pub mod serena_install;
