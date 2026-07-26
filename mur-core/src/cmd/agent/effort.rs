//! `mur agent effort` — set, show, or clear an agent's per-turn effort.
//!
//! Effort is the Anthropic `output_config.effort` scale. The value lives on the
//! agent profile because it describes the agent's JOB: a single-purpose build
//! specialist earns `xhigh`, a fan-out research worker `medium`, a classifier
//! `low`. Individual mechanical calls inside the runtime cap themselves lower
//! regardless of what is set here.
//!
//! Leaving it unset is not the same as "no effort" — the API default is
//! `high`, so every unset agent is already paying for high effort.

use anyhow::{Result, bail};
use mur_common::llm::Effort;

use super::perm::warn_if_running;
use super::{load_profile_for_edit, save_profile};

/// `mur agent effort <name> [level] [--clear]`
///
/// With no level and no `--clear`, prints the current setting.
pub fn cmd_effort(name: &str, level: Option<String>, clear: bool) -> Result<()> {
    if clear && level.is_some() {
        bail!("pass a level or --clear, not both");
    }

    let (path, mut profile) = load_profile_for_edit(name)?;

    // Read-only form: report and stop, so `effort <name>` is a safe thing to
    // type when you're not sure what an agent is on.
    if !clear && level.is_none() {
        match profile.effort {
            Some(e) => println!("{name}: {}", e.as_str()),
            None => println!("{name}: (unset — the API default is high)"),
        }
        return Ok(());
    }

    let new = match level {
        Some(raw) => Some(raw.parse::<Effort>().map_err(|e| anyhow::anyhow!(e))?),
        None => None,
    };

    if profile.effort == new {
        match new {
            Some(e) => println!("{name}: already {}", e.as_str()),
            None => println!("{name}: already unset"),
        }
        return Ok(());
    }

    profile.effort = new;
    save_profile(&path, &mut profile)?;
    match new {
        Some(e) => println!("{name}: effort set to {}", e.as_str()),
        None => println!("{name}: effort cleared (the API default is high)"),
    }
    // The profile is read once at startup, so a running agent keeps the old
    // value until it restarts.
    warn_if_running(name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use mur_common::llm::Effort;

    #[test]
    fn every_level_round_trips_through_the_cli_string() {
        // The CLI takes a bare string; if parsing and rendering ever disagree
        // a level becomes unsettable, with no error to show for it.
        for e in Effort::ALL {
            assert_eq!(e.as_str().parse::<Effort>().unwrap(), *e);
        }
    }

    #[test]
    fn unknown_level_lists_the_valid_ones() {
        let err = "hgih".parse::<Effort>().unwrap_err();
        assert!(err.contains("hgih"), "{err}");
        for e in Effort::ALL {
            assert!(err.contains(e.as_str()), "{err} should list {}", e.as_str());
        }
    }

    #[test]
    fn parsing_is_case_and_space_insensitive() {
        assert_eq!("  XHigh ".parse::<Effort>().unwrap(), Effort::Xhigh);
    }
}
