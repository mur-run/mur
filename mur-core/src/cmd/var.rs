//! CLI commands for `mur var` — manage user-defined variables.

use anyhow::Result;
use mur_common::variable::VariableStore;

/// List all variables (global + active profile).
pub(crate) fn cmd_var_list(profile: Option<&str>) -> Result<()> {
    let store = VariableStore::load()?;

    println!("🔧 Variables (active profile: {})\n", store.active_profile);

    // Show global variables
    if !store.global.is_empty() {
        println!("  Global:");
        for (k, v) in &store.global {
            println!("    {{{{{}}}}} = {}", k, v);
        }
        println!();
    }

    // Show profile variables
    let target_profile = profile.unwrap_or(&store.active_profile);
    if let Some(profile_vars) = store.profiles.get(target_profile)
        && !profile_vars.is_empty()
    {
        println!("  Profile [{}]:", target_profile);
        for (k, v) in profile_vars {
            println!("    {{{{{}}}}} = {}", k, v);
        }
        println!();
    }

    // Show effective (merged) variables
    let effective = store.effective_vars();
    if !effective.is_empty() {
        println!("  Effective (merged):");
        for (k, v) in &effective {
            println!("    {{{{{}}}}} = {}", k, v);
        }
    } else {
        println!("  No variables defined. Use `mur var set <name> <value>` to add one.");
    }

    Ok(())
}

/// Set a variable.
pub(crate) fn cmd_var_set(name: &str, value: &str, profile: Option<&str>) -> Result<()> {
    let mut store = VariableStore::load()?;

    if let Some(profile) = profile {
        store.set_profile(profile, name, value);
        println!("✓ Set {{{{{}}}}} = {} (profile: {})", name, value, profile);
    } else {
        store.set_global(name, value);
        println!("✓ Set {{{{{}}}}} = {} (global)", name, value);
    }

    store.save()?;
    Ok(())
}

/// Get a variable's value.
pub(crate) fn cmd_var_get(name: &str) -> Result<()> {
    let store = VariableStore::load()?;
    let effective = store.effective_vars();

    if let Some(val) = effective.get(name) {
        println!("{}", val);
    } else {
        eprintln!("Variable '{}' not found", name);
        std::process::exit(1);
    }
    Ok(())
}

/// Delete a variable.
pub(crate) fn cmd_var_delete(name: &str, profile: Option<&str>) -> Result<()> {
    let mut store = VariableStore::load()?;

    let removed = if let Some(profile) = profile {
        store.remove_profile(profile, name)
    } else {
        store.remove_global(name)
    };

    if removed {
        store.save()?;
        println!("✓ Removed {{{{{}}}}}", name);
    } else {
        eprintln!("Variable '{}' not found", name);
        std::process::exit(1);
    }
    Ok(())
}

/// Switch active profile.
pub(crate) fn cmd_var_profile(profile: &str) -> Result<()> {
    let mut store = VariableStore::load()?;
    store.switch_profile(profile);
    store.save()?;
    println!("✓ Switched to profile: {}", profile);
    Ok(())
}

/// List all profiles.
pub(crate) fn cmd_var_profiles() -> Result<()> {
    let store = VariableStore::load()?;
    println!("📋 Variable Profiles:\n");

    if store.profiles.is_empty() && store.global.is_empty() {
        println!("  No profiles or variables defined.");
        return Ok(());
    }

    for name in store.profile_names() {
        let active = if name == store.active_profile {
            " (active)"
        } else {
            ""
        };
        let count = store.profiles.get(name).map(|v| v.len()).unwrap_or(0);
        println!("  {}{} — {} variables", name, active, count);
    }

    println!("\n  Global: {} variables", store.global.len());
    Ok(())
}
