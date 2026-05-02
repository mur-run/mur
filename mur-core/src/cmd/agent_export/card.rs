//! `build_card_from_profile` — pure helper that projects an `AgentProfile`
//! into a `MurCard`. The full `mur agent export --format card` pipeline is
//! a D4 milestone; M2.7 only lands this helper so D4 doesn't have to
//! retrofit first_memory plumbing later.
//!
//! Spec: `docs/superpowers/specs/2026-04-30-mur-agent-d2-onboarding-design.md`
//! §4.4.

use crate::character_card::{extensions::MurExt, first_memory::FirstMemoryExt, schema::*};
use mur_common::agent::AgentProfile;

/// Build a `MurCard` from an agent profile, threading the companion
/// onboarding's `first_memory` into `extensions.mur.first_memory` when
/// present. The card's `data.name` prefers the onboarding display name and
/// falls back to the agent's profile `name`.
///
/// `--format card` is a D4 milestone; the `mur agent companion card export`
/// CLI (D4 M4.7) and the integration tests in
/// `mur-core/tests/companion_first_memory_to_card.rs` are the current
/// callers.
pub fn build_card_from_profile(p: &AgentProfile) -> MurCard {
    let first_memory = p
        .companion
        .onboarding
        .first_memory
        .as_ref()
        .map(|fm| FirstMemoryExt {
            text: fm.text.clone(),
            established_at: fm.established_at,
        });
    MurCard {
        spec: "murcard_v1".into(),
        spec_version: "1.0".into(),
        data: CardData {
            name: p
                .companion
                .onboarding
                .agent_display_name
                .clone()
                .unwrap_or_else(|| p.name.clone()),
            ..Default::default()
        },
        extensions: first_memory.map(|fm| Extensions {
            mur: Some(MurExt {
                first_memory: Some(fm),
                ..Default::default()
            }),
        }),
        ccv3_passthrough: Default::default(),
    }
}
