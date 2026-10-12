//! Writers for the `limits:` block, one scope each (spec 2026-09-12 §3.9).
//! Fleet and agent go through their typed stores. The GLOBAL scope is its own
//! file, `<mur_home>/limits/global.yaml` (issue #1794); the legacy
//! `config.yaml limits:` block is read as a fallback and never written.

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
    println!("limits for agent '{name}' updated — takes effect on its next turn");
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

/// The global scope's file body: the block without its `limits:` wrapper.
/// An empty scope is written as `{}` — the file must still exist so the
/// legacy `config.yaml` block stays shadowed after the user unsets every key.
pub fn render_global_file(l: Option<&Limits>) -> String {
    match l {
        Some(l) if !l.is_empty() => render_limits_block(l)
            .lines()
            .skip(1)
            .map(|line| format!("{}\n", line.trim_start()))
            .collect(),
        _ => "{}\n".to_string(),
    }
}

/// Write the global scope to `<mur_home>/limits/global.yaml` (issue #1794),
/// atomically. The first write seeds from the legacy `config.yaml limits:`
/// block so the user edits what `mur limits` showed them; `config.yaml`
/// itself is never touched — the legacy block stays where it is and stops
/// being read once this file exists. A failed write changes nothing.
pub fn upsert_global_limits(mur_home: &Path, patch: &Patch) -> Result<()> {
    use mur_common::limits::{GlobalSource, global_limits_dir, global_limits_path, load_global};
    let current = load_global(mur_home).map_err(|e| anyhow!("{e}"))?;
    let migrating = current.source == GlobalSource::Legacy && !current.limits.is_empty();
    let seed = (!current.limits.is_empty()).then_some(current.limits);
    let next = apply_patch(seed, patch).map_err(|e| anyhow!("{e}"))?;

    let dir = global_limits_dir(mur_home);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = global_limits_path(mur_home);
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, render_global_file(next.as_ref()))
        .with_context(|| format!("write {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("rename to {}", path.display()));
    }
    println!("global limits updated in {}", path.display());
    if migrating {
        println!(
            "  migrated from {} — its limits: block is now ignored (left in place; delete it by hand if you like)",
            mur_common::limits::legacy_config_path(mur_home).display()
        );
    }
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

    fn patch(deadline: Option<&str>, stuck: Option<&str>, unset: &[&str]) -> Patch {
        Patch {
            deadline: deadline.map(Into::into),
            stuck: stuck.map(Into::into),
            cost_usd: None,
            unset: unset.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// #1794: the global writer owns `limits/global.yaml` and nothing else.
    /// The first write migrates the legacy values and leaves `config.yaml`
    /// byte-for-byte as the user wrote it — comments, foreign blocks and the
    /// old `limits:` block included.
    #[test]
    fn first_global_write_migrates_and_leaves_config_yaml_untouched() {
        use mur_common::limits::{GlobalSource, global_limits_path, load_global};
        let t = tempfile::tempdir().unwrap();
        let cfg = t.path().join("config.yaml");
        let original = "# hand-kept\nresearch_gateway:\n  brave_api_key_ref: keychain:mur/brave\nlimits:\n  deadline: 4h  # why: long builds\n  cost_usd: 2\nfleet_run:\n  agents: [mur]\n";
        std::fs::write(&cfg, original).unwrap();

        upsert_global_limits(t.path(), &patch(None, Some("15m"), &[])).unwrap();

        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), original);
        let g = load_global(t.path()).unwrap();
        assert_eq!(g.source, GlobalSource::File);
        assert_eq!(g.limits.deadline.as_deref(), Some("4h"), "migrated");
        assert_eq!(g.limits.cost_usd, Some(2.0), "migrated");
        assert_eq!(g.limits.stuck.as_deref(), Some("15m"), "patched");
        let body = std::fs::read_to_string(global_limits_path(t.path())).unwrap();
        assert_eq!(body, "deadline: 4h\nstuck: 15m\ncost_usd: 2\n");
    }

    /// Once the new file exists, later writes edit it alone, and unsetting
    /// every key keeps an empty file so the legacy block stays shadowed.
    #[test]
    fn later_writes_edit_the_new_file_and_an_empty_scope_still_shadows() {
        use mur_common::limits::load_global;
        let t = tempfile::tempdir().unwrap();
        let cfg = t.path().join("config.yaml");
        std::fs::write(&cfg, "limits:\n  deadline: 9h\n").unwrap();
        upsert_global_limits(t.path(), &patch(Some("1h"), None, &[])).unwrap();
        upsert_global_limits(t.path(), &patch(None, Some("off"), &[])).unwrap();
        let g = load_global(t.path()).unwrap().limits;
        assert_eq!(g.deadline.as_deref(), Some("1h"));
        assert_eq!(g.stuck.as_deref(), Some("off"));

        upsert_global_limits(t.path(), &patch(None, None, &["deadline", "stuck"])).unwrap();
        assert!(load_global(t.path()).unwrap().limits.is_empty());
        assert_eq!(
            std::fs::read_to_string(&cfg).unwrap(),
            "limits:\n  deadline: 9h\n"
        );
    }

    /// A write that cannot land changes nothing: no new file, `config.yaml`
    /// intact, and a bad value is refused before any disk work.
    #[test]
    fn a_failed_global_write_changes_nothing() {
        use mur_common::limits::{global_limits_dir, global_limits_path};
        let t = tempfile::tempdir().unwrap();
        let cfg = t.path().join("config.yaml");
        std::fs::write(&cfg, "limits:\n  deadline: 9h\n").unwrap();

        assert!(upsert_global_limits(t.path(), &patch(Some("soon"), None, &[])).is_err());
        assert!(!global_limits_dir(t.path()).exists());

        // A plain file where the directory must go: create_dir_all fails.
        std::fs::write(global_limits_dir(t.path()), "not a dir").unwrap();
        assert!(upsert_global_limits(t.path(), &patch(Some("1h"), None, &[])).is_err());
        assert!(!global_limits_path(t.path()).exists());
        assert_eq!(
            std::fs::read_to_string(&cfg).unwrap(),
            "limits:\n  deadline: 9h\n"
        );
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
            procedure: vec![],
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
        let mut envg = mur_common::test_env::EnvGuard::hold();
        envg.set_var("MUR_HOME", home);
        remove_stale_key(
            home,
            &crate::cmd::limits::Target::Agent("pm".into()),
            "hitl.max_tokens",
        )
        .unwrap();
        envg.unset_var("MUR_HOME");
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
