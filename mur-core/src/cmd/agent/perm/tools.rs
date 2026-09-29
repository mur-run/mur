//! `mur agent perm` tool policy rules.

use anyhow::Result;
use mur_common::agent::{ToolPolicy, ToolRule};

use super::super::{load_profile_for_edit, save_profile};
use super::warn_if_running;

pub fn cmd_perm_set_tool(name: &str, policy: ToolPolicy, pattern: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let rules = &mut profile.entitlements.tools;
    if let Some(r) = rules.iter_mut().find(|r| r.pattern == pattern) {
        r.policy = policy;
    } else {
        rules.push(ToolRule {
            pattern: pattern.to_string(),
            policy,
            risk: None,
        });
    }
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

pub fn cmd_perm_clear_tool(name: &str, pattern: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    profile.entitlements.tools.retain(|r| r.pattern != pattern);
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

pub fn cmd_perm_list_tools(name: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    let rules = &profile.entitlements.tools;
    if rules.is_empty() {
        println!("(no tool rules — all tools use default policy: ask)");
    } else {
        for r in rules {
            println!(
                "{:10}  {}",
                format!("{:?}", r.policy).to_lowercase(),
                r.pattern
            );
        }
    }
    Ok(())
}
