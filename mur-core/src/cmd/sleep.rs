use anyhow::Result;

use crate::store::config::{load_config, save_config};

pub fn cmd_sleep_enable() -> Result<()> {
    let mut cfg = load_config()?;
    if cfg.sleep_cycle.enabled {
        println!("Sleep cycle already enabled.");
        return Ok(());
    }
    cfg.sleep_cycle.enabled = true;
    save_config(&cfg)?;
    println!(
        "Sleep cycle enabled (idle threshold: {}min).",
        cfg.sleep_cycle.idle_threshold_minutes
    );
    Ok(())
}

pub fn cmd_sleep_disable() -> Result<()> {
    let mut cfg = load_config()?;
    if !cfg.sleep_cycle.enabled {
        println!("Sleep cycle already disabled.");
        return Ok(());
    }
    cfg.sleep_cycle.enabled = false;
    save_config(&cfg)?;
    println!("Sleep cycle disabled.");
    Ok(())
}

pub fn cmd_sleep_status() -> Result<()> {
    let cfg = load_config()?;
    let sc = &cfg.sleep_cycle;
    let state = if sc.enabled { "enabled" } else { "disabled" };
    println!("sleep cycle     : {state}");
    println!("idle threshold  : {}min (daemon)", sc.idle_threshold_minutes);
    println!("agent idle      : {}min (agent-side)", sc.agent_idle_minutes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn with_tmp_mur_home(f: impl FnOnce()) {
        let tmp = TempDir::new().unwrap();
        // SAFETY: test-only, single-threaded test runner per process.
        unsafe { std::env::set_var("MUR_HOME", tmp.path()) };
        f();
        unsafe { std::env::remove_var("MUR_HOME") };
    }

    #[test]
    fn enable_writes_config() {
        with_tmp_mur_home(|| {
            let cfg_before = load_config().unwrap();
            assert!(!cfg_before.sleep_cycle.enabled);
            cmd_sleep_enable().unwrap();
            let cfg_after = load_config().unwrap();
            assert!(cfg_after.sleep_cycle.enabled);
        });
    }

    #[test]
    fn disable_writes_config() {
        with_tmp_mur_home(|| {
            cmd_sleep_enable().unwrap();
            cmd_sleep_disable().unwrap();
            let cfg = load_config().unwrap();
            assert!(!cfg.sleep_cycle.enabled);
        });
    }

    #[test]
    fn status_reads_defaults() {
        with_tmp_mur_home(|| {
            cmd_sleep_status().unwrap();
        });
    }
}
