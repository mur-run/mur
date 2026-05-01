//! `card export` — render an agent's profile as a `.murcard.yaml` (or
//! signed variant).
//!
//! WHY: `card import`/`accept` (M4.5/M4.6) cover the inbound side of the D4
//! pipeline, but operators also need a supported outbound path so an agent
//! can be exchanged with a peer without hand-crafting YAML or shelling
//! through `mur agent export --format card` (a separate D4 milestone). This
//! module wires the existing `build_card_from_profile` projection into a
//! file-writing entry point and threads the agent's identity key through
//! `sign_card` when the caller asks for a signed export. Keeping the
//! function shape small (`agent`, `output`, `sign`) lets the CLI hand it
//! arguments verbatim and keeps the door open for `--format card` to call
//! the same helper later.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::character_card::schema::MurCard;
use crate::character_card::signing::sign_card;
use crate::cmd::agent_export::card::build_card_from_profile;
use mur_common::agent::AgentProfile;
use mur_common::identity::AgentIdentity;

/// Export the named agent's profile as a `.murcard.yaml`.
///
/// WHY this signature: `output` defaults to `<agent_home>/<name>.murcard.yaml`
/// so a bare `card export <name>` keeps the artifact next to the agent it
/// describes. `sign` is opt-in — unsigned exports are valid CCv3 and let
/// quick local round-trips skip the keychain dance, while signed exports
/// give peers a way to verify provenance via `verify_card`. We sign AFTER
/// `build_card_from_profile` and BEFORE the YAML serialize so the on-disk
/// bytes are exactly what `verify_card` will canonicalize and check.
///
/// `#[allow(dead_code)]` lasts only until the M4.7.2 commit wires this into
/// `cli.rs`; the binary target sees no caller until then.
#[allow(dead_code)]
pub async fn export_card(agent: &str, output: Option<&Path>, sign: bool) -> Result<PathBuf> {
    let agent_home = super::super::util::agent_home_for(agent)?;
    let profile_path = agent_home.join("profile.yaml");
    let profile: AgentProfile = serde_yaml_ng::from_str(
        &std::fs::read_to_string(&profile_path)
            .with_context(|| format!("read {}", profile_path.display()))?,
    )
    .context("parse profile.yaml")?;
    let mut card: MurCard = build_card_from_profile(&profile);

    if sign {
        let identity =
            AgentIdentity::load(&agent_home).context("load identity.key for signing")?;
        sign_card(&mut card, identity.signing_key()).context("sign card")?;
    }

    let yaml = serde_yaml_ng::to_string(&card).context("serialize card yaml")?;
    let path = match output {
        Some(p) => p.to_path_buf(),
        None => agent_home.join(format!("{agent}.murcard.yaml")),
    };
    std::fs::write(&path, yaml).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}
