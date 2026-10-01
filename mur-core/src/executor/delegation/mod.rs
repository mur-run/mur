//! Delegation plumbing shared by every fan-out path (`mur fleet run`,
//! `parallel_jobs`, workflow `delegate_to`): where the work is and, later,
//! whether the member may write there. See
//! `docs/superpowers/specs/2026-10-01-delegation-write-grant-design.md`.

pub mod cwd;
pub mod grant;
