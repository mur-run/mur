//! Writers for the `limits:` block, one scope each (spec 2026-09-12 §3.9).
//! Fleet and agent go through their typed stores. The GLOBAL scope is
//! `config.yaml`, and that file is edited as text: the typed `Config` drops
//! blocks other binaries own (`research_gateway:` is the known casualty), so
//! load-modify-save there is a data-loss bug, not a shortcut.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use mur_common::limits::{Limits, validate};

/// What one command invocation changes. `unset` names keys to remove.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Patch {
    pub deadline: Option<String>,
    pub stuck: Option<String>,
    pub cost_usd: Option<f64>,
    pub unset: Vec<String>,
}

const KEYS: [&str; 3] = ["deadline", "stuck", "cost_usd"];

/// Pure: the block after the patch, validated. `Ok(None)` means "no keys
/// left — drop the block so the scope inherits".
pub fn apply_patch(current: Option<Limits>, patch: &Patch) -> Result<Option<Limits>, String> {
    let mut l = current.unwrap_or_default();
    for k in &patch.unset {
        match k.as_str() {
            "deadline" => l.deadline = None,
            "stuck" => l.stuck = None,
            "cost_usd" => l.cost_usd = None,
            other => {
                return Err(format!(
                    "`{other}` is not a limits key (one of: {})",
                    KEYS.join(", ")
                ));
            }
        }
    }
    if let Some(d) = &patch.deadline {
        l.deadline = Some(d.trim().to_string());
    }
    if let Some(s) = &patch.stuck {
        l.stuck = Some(s.trim().to_string());
    }
    if let Some(c) = patch.cost_usd {
        l.cost_usd = Some(c);
    }
    validate(&l)?;
    Ok((!l.is_empty()).then_some(l))
}

pub fn write_fleet_limits(mur_home: &Path, name: &str, patch: &Patch) -> Result<()> {
    let mut fleet = crate::cmd::fleet::store::load_fleet(mur_home, name)?;
    // A legacy fleet's first write materialises the derived block, so the
    // user edits what `mur limits` showed them, not a hidden loop.* field.
    let current = fleet.limits_or_legacy();
    fleet.limits = apply_patch(current, patch).map_err(|e| anyhow!("{e}"))?;
    crate::cmd::fleet::store::save_fleet(mur_home, &fleet)?;
    println!("limits for fleet '{name}' updated — takes effect on its next run");
    Ok(())
}

pub fn write_agent_limits(name: &str, patch: &Patch) -> Result<()> {
    let (path, mut profile) = crate::cmd::agent::load_profile_for_edit(name)?;
    profile.limits = apply_patch(profile.limits.take(), patch).map_err(|e| anyhow!("{e}"))?;
    crate::cmd::agent::save_profile(&path, &mut profile)?;
    println!(
        "limits for agent '{name}' updated — restart the agent to apply (mur agent restart {name})"
    );
    Ok(())
}

/// YAML for the block, keys in schema order, numbers without a trailing `.0`.
pub fn render_limits_block(l: &Limits) -> String {
    let mut out = String::from("limits:\n");
    if let Some(d) = &l.deadline {
        out.push_str(&format!("  deadline: {d}\n"));
    }
    if let Some(s) = &l.stuck {
        out.push_str(&format!("  stuck: {s}\n"));
    }
    if let Some(c) = l.cost_usd {
        let c = if c.fract() == 0.0 {
            format!("{}", c as i64)
        } else {
            format!("{c}")
        };
        out.push_str(&format!("  cost_usd: {c}\n"));
    }
    out
}

/// Textual upsert of the top-level `limits:` block in `config.yaml`. The
/// block is the lines from `limits:` up to the next top-level key (a line
/// that starts in column 0 and is not blank or a comment). Everything else is
/// preserved byte-for-byte.
pub fn upsert_global_limits(config_path: &Path, patch: &Patch) -> Result<()> {
    let text = std::fs::read_to_string(config_path).unwrap_or_default();
    let current = mur_common::config::Config::load_or_default(config_path).limits;
    let next =
        apply_patch((!current.is_empty()).then_some(current), patch).map_err(|e| anyhow!("{e}"))?;

    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim_end() == "limits:" || l.starts_with("limits:"));
    let (before, after): (Vec<&str>, Vec<&str>) = match start {
        Some(s) => {
            let mut e = s + 1;
            while e < lines.len() {
                let l = lines[e];
                let top_level = !l.is_empty()
                    && !l.starts_with(' ')
                    && !l.starts_with('\t')
                    && !l.starts_with('#');
                if top_level {
                    break;
                }
                e += 1;
            }
            (lines[..s].to_vec(), lines[e..].to_vec())
        }
        None => (lines.clone(), Vec::new()),
    };
    let mut out = before.join("\n");
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if let Some(l) = &next {
        out.push_str(&render_limits_block(l));
    }
    if !after.is_empty() {
        out.push_str(&after.join("\n"));
        out.push('\n');
    }
    let tmp = config_path.with_extension("yaml.tmp");
    std::fs::write(&tmp, out).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, config_path)
        .with_context(|| format!("rename to {}", config_path.display()))?;
    println!("global limits updated in {}", config_path.display());
    Ok(())
}

/// Delete one legacy key through the same store the edits use (spec §10.4):
/// the fleet's loop.* through save_fleet, the agent's hitl.* through
/// save_profile. Anything else is refused by name so a typo cannot clear a
/// live setting.
///
/// Called by mur-hub-gui's `limits_remove_stale` Tauri command (Hub 3b) — the
/// CLI has no equivalent subcommand yet, so the bin target never reaches it.
#[allow(dead_code)]
pub fn remove_stale_key(
    mur_home: &Path,
    target: &crate::cmd::limits::Target,
    key: &str,
) -> Result<()> {
    use crate::cmd::limits::Target;
    match (target, key) {
        (Target::Fleet(name), "loop.max_iterations" | "loop.budget_usd" | "loop.deadline") => {
            let mut fleet = crate::cmd::fleet::store::load_fleet(mur_home, name)?;
            if let Some(lc) = fleet.loop_cfg.as_mut() {
                match key {
                    "loop.max_iterations" => lc.max_iterations = 0,
                    "loop.budget_usd" => lc.budget_usd = 0.0,
                    _ => lc.deadline.clear(),
                }
            }
            crate::cmd::fleet::store::save_fleet(mur_home, &fleet)
        }
        (Target::Agent(name), "hitl.max_iterations" | "hitl.max_tokens") => {
            let (path, mut profile) = crate::cmd::agent::load_profile_for_edit(name)?;
            if key == "hitl.max_iterations" {
                profile.hitl.max_iterations = None;
            } else {
                profile.hitl.max_tokens = None;
            }
            crate::cmd::agent::save_profile(&path, &mut profile)
        }
        (Target::Global, _) => anyhow::bail!("config.yaml has no stale limits keys"),
        (Target::Fleet(_), other) => anyhow::bail!(
            "`{other}` is not a stale fleet key (loop.max_iterations, loop.budget_usd, loop.deadline)"
        ),
        (Target::Agent(_), other) => anyhow::bail!(
            "`{other}` is not a stale agent key (hitl.max_iterations, hitl.max_tokens)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::limits::Limits;

    #[test]
    fn a_patch_sets_and_unsets_keys_and_validates() {
        let p = Patch {
            deadline: Some("2h".into()),
            stuck: None,
            cost_usd: Some(5.0),
            unset: vec![],
        };
        let l = apply_patch(None, &p).unwrap().unwrap();
        assert_eq!(l.deadline.as_deref(), Some("2h"));
        assert_eq!(l.cost_usd, Some(5.0));
        let p2 = Patch {
            deadline: None,
            stuck: Some("off".into()),
            cost_usd: None,
            unset: vec!["cost_usd".into()],
        };
        let l2 = apply_patch(Some(l), &p2).unwrap().unwrap();
        assert_eq!(l2.cost_usd, None);
        assert_eq!(l2.stuck.as_deref(), Some("off"));
        assert_eq!(l2.deadline.as_deref(), Some("2h"), "untouched keys survive");
        // Unsetting the last key leaves no block (None), so the scope inherits.
        let p3 = Patch {
            deadline: None,
            stuck: None,
            cost_usd: None,
            unset: vec!["deadline".into(), "stuck".into()],
        };
        assert_eq!(apply_patch(Some(l2), &p3).unwrap(), None);
        // Bad values are refused before anything is written.
        let bad = Patch {
            deadline: Some("soon".into()),
            stuck: None,
            cost_usd: None,
            unset: vec![],
        };
        assert!(
            apply_patch(None, &bad)
                .unwrap_err()
                .contains("limits.deadline")
        );
        let unknown = Patch {
            deadline: None,
            stuck: None,
            cost_usd: None,
            unset: vec!["max_iterations".into()],
        };
        assert!(
            apply_patch(None, &unknown)
                .unwrap_err()
                .contains("max_iterations")
        );
    }

    /// The global writer edits config.yaml as TEXT: blocks it does not own
    /// survive byte-for-byte, an existing limits: block is replaced in place,
    /// and a missing one is appended.
    #[test]
    fn global_write_is_textual_and_preserves_foreign_blocks() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("config.yaml");
        std::fs::write(&p, "research_gateway:\n  brave_api_key_ref: keychain:mur/brave\nfleet_run:\n  agents: [mur]\n").unwrap();
        upsert_global_limits(
            &p,
            &Patch {
                deadline: Some("4h".into()),
                stuck: None,
                cost_usd: None,
                unset: vec![],
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(
            text.contains("research_gateway:\n  brave_api_key_ref: keychain:mur/brave\n"),
            "{text}"
        );
        assert!(text.contains("fleet_run:\n  agents: [mur]\n"), "{text}");
        assert!(text.contains("limits:\n  deadline: 4h\n"), "{text}");
        // Replace in place, not append twice.
        upsert_global_limits(
            &p,
            &Patch {
                deadline: None,
                stuck: Some("15m".into()),
                cost_usd: None,
                unset: vec![],
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text.matches("limits:").count(), 1, "{text}");
        assert!(
            text.contains("  deadline: 4h\n") && text.contains("  stuck: 15m\n"),
            "{text}"
        );
        // The typed loader agrees with the text.
        let cfg = mur_common::config::Config::load_or_default(&p);
        assert_eq!(cfg.limits.deadline.as_deref(), Some("4h"));
        assert_eq!(cfg.limits.stuck.as_deref(), Some("15m"));
        // Unsetting every key removes the block entirely.
        upsert_global_limits(
            &p,
            &Patch {
                deadline: None,
                stuck: None,
                cost_usd: None,
                unset: vec!["deadline".into(), "stuck".into()],
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("limits:"), "{text}");
        assert!(text.contains("research_gateway:"), "{text}");
    }

    fn fleet_for_test(
        name: &str,
        loop_cfg: Option<mur_common::fleet::FleetLoop>,
    ) -> mur_common::fleet::Fleet {
        mur_common::fleet::Fleet {
            name: name.into(),
            display_name: String::new(),
            goal: "g".into(),
            router: None,
            team_id: None,
            members: vec!["pm".into()],
            channel_id: format!("fleet-{name}"),
            rules: vec![],
            skills: vec![],
            loop_cfg,
            parallel: None,
            hitl: None,
            requires_programs: vec![],
            limits: None,
            needs: vec![],
        }
    }

    #[test]
    fn remove_stale_key_clears_exactly_that_key() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path();
        let f = fleet_for_test(
            "dev",
            Some(mur_common::fleet::FleetLoop {
                trigger: "manual".into(),
                max_iterations: 8,
                budget_usd: 5.0,
                deadline: "2h".into(),
                done_when: String::new(),
            }),
        );
        crate::cmd::fleet::store::save_fleet(home, &f).unwrap();
        remove_stale_key(
            home,
            &crate::cmd::limits::Target::Fleet("dev".into()),
            "loop.max_iterations",
        )
        .unwrap();
        let back = crate::cmd::fleet::store::load_fleet(home, "dev").unwrap();
        assert_eq!(back.loop_cfg.as_ref().unwrap().max_iterations, 0);
        assert_eq!(
            back.loop_cfg.as_ref().unwrap().budget_usd,
            5.0,
            "the other keys survive"
        );

        let mut yaml = std::fs::read_to_string(
            env!("CARGO_MANIFEST_DIR").to_string()
                + "/../mur-hub-gui/src-tauri/resources/mur-agent-template/profile.yaml",
        )
        .unwrap()
        .replacen(
            "name: mur
",
            "name: pm
model_ref: local
",
            1,
        );
        yaml.push_str(
            "hitl:
  max_iterations: 800
  max_tokens: 9
",
        );
        std::fs::create_dir_all(home.join("agents/pm")).unwrap();
        std::fs::write(home.join("agents/pm/profile.yaml"), yaml).unwrap();
        unsafe { std::env::set_var("MUR_HOME", home) };
        remove_stale_key(
            home,
            &crate::cmd::limits::Target::Agent("pm".into()),
            "hitl.max_tokens",
        )
        .unwrap();
        unsafe { std::env::remove_var("MUR_HOME") };
        let p = mur_common::agent::AgentProfile::load(home, "pm").unwrap();
        assert_eq!(p.hitl.max_tokens, None);
        assert_eq!(p.hitl.max_iterations, Some(800));

        let e = remove_stale_key(
            home,
            &crate::cmd::limits::Target::Fleet("dev".into()),
            "loop.trigger",
        )
        .unwrap_err()
        .to_string();
        assert!(
            e.contains("loop.max_iterations"),
            "names the allowed keys: {e}"
        );

        let e = remove_stale_key(home, &crate::cmd::limits::Target::Global, "anything")
            .unwrap_err()
            .to_string();
        assert!(e.contains("no stale"), "{e}");
    }

    #[test]
    fn render_block_is_stable_yaml() {
        let l = Limits {
            deadline: Some("2h".into()),
            stuck: Some("off".into()),
            cost_usd: Some(5.0),
        };
        assert_eq!(
            render_limits_block(&l),
            "limits:\n  deadline: 2h\n  stuck: off\n  cost_usd: 5\n"
        );
        let l = Limits {
            deadline: None,
            stuck: None,
            cost_usd: Some(0.5),
        };
        assert_eq!(render_limits_block(&l), "limits:\n  cost_usd: 0.5\n");
    }
}
