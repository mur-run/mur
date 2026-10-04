//! `mur code-nav setup` (plan 2026-10-03 code-nav, Phase 3; decision P3-D1:
//! a standalone command shaped like `mur browser setup`).
//!
//! Only the pure planner (task 3.1) exists so far. Installers, the serena
//! config generator, the profile entry and the consent flow (3.2–3.6) build
//! on [`plan::Plan`].

pub mod plan;
