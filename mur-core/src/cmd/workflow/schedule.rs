//! `mur workflow schedule ...` backed by `~/.mur/schedules.yaml`.

use super::*;

// ─── Schedule management (uses ~/.mur/schedules.yaml) ──────────────────────

use mur_common::schedule::{Schedule, ScheduleNotify, SchedulesFile};

fn schedules_path() -> std::path::PathBuf {
    crate::paths::mur_root(None).join("schedules.yaml")
}

fn load_schedules() -> Result<Vec<Schedule>> {
    let path = schedules_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)?;
    let file: SchedulesFile = serde_yaml::from_str(&content)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(file.schedules)
}

fn save_schedules(schedules: &[Schedule]) -> Result<()> {
    let path = schedules_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = SchedulesFile {
        schedules: schedules.to_vec(),
    };
    let yaml = serde_yaml::to_string(&file)?;
    std::fs::write(&path, yaml)?;
    Ok(())
}

pub(crate) fn cmd_schedule_list() -> Result<()> {
    let schedules = load_schedules()?;

    if schedules.is_empty() {
        println!("📋 No scheduled workflows.");
        println!("  Use `mur workflow schedule set <name> \"0 * * * *\"` to add one.");
        return Ok(());
    }

    let active: Vec<_> = schedules.iter().filter(|s| s.enabled).collect();
    let disabled: Vec<_> = schedules.iter().filter(|s| !s.enabled).collect();

    if !active.is_empty() {
        println!("🔄 Active schedules:\n");
        // Show the zone the schedule will ACTUALLY fire in, not the stored
        // label: launchd and crontab both read the cron expression as local
        // time regardless of what `timezone` says. Entries written before this
        // was fixed all carry a hardcoded "UTC" that never applied.
        let local_tz = iana_time_zone::get_timezone().unwrap_or_else(|_| "local".to_string());
        for s in &active {
            println!("  {} — `{}` ({})", s.workflow, s.cron, local_tz);
            if s.timezone != local_tz {
                println!("     stored timezone: {} (not used locally)", s.timezone);
            }
            if !s.user_id.is_empty() {
                println!("     user: {}", s.user_id);
            }
            if !s.notify.target.is_empty() {
                println!(
                    "     notify: {} → {}",
                    s.notify.notify_type, s.notify.target
                );
            }
        }
    }

    if !disabled.is_empty() {
        println!("\n⏸️  Disabled:\n");
        for s in &disabled {
            println!("  {} — `{}`", s.workflow, s.cron);
        }
    }

    println!("\n  Total: {} schedule(s)", schedules.len());

    // Show system-level schedule status
    let system = crate::cmd::system_schedule::list_system_schedules();
    if !system.is_empty() {
        println!("\n🖥️  System schedules:");
        for (name, backend) in &system {
            println!("  {} ({})", name, backend);
        }
    }
    Ok(())
}

pub(crate) fn cmd_schedule_set(name: &str, cron: &str) -> Result<()> {
    // Validate cron
    let parts: Vec<&str> = cron.split_whitespace().collect();
    if parts.len() < 5 || parts.len() > 7 {
        anyhow::bail!(
            "Invalid cron expression '{}'. Expected 5-7 fields (e.g. '0 * * * *' for hourly).",
            cron
        );
    }

    // Check for stale Commander PID — if PID file exists but process is dead, reclaim for system cron
    if mur_common::schedule_claim::commander_pid_path().exists()
        && !mur_common::schedule_claim::is_commander_running()
        && let Ok(released) = mur_common::schedule_claim::release_all_from_commander()
        && !released.is_empty()
    {
        eprintln!(
            "   ⚠️  Commander not running (stale PID). Reclaiming {} schedule(s) for system cron.",
            released.len()
        );
    }

    // Verify the name resolves to something the scheduler can actually fire.
    // A schedule runs `mur run <name>`, which resolves flat workflows AND
    // exact-named workflow skills — so both are valid here.
    let store = WorkflowYamlStore::default_store()?;
    if store.get(name).is_err()
        && find_workflow_skill(&crate::paths::mur_root(None), name).is_none()
    {
        anyhow::bail!(
            "Workflow '{}' not found — no ~/.mur/workflows/{}.yaml, and no `category: Workflow` \
             skill of that name with a `content.procedure`.",
            name,
            name
        );
    }

    // Determine executor: Commander if running, otherwise system cron
    let executor = mur_common::schedule_claim::auto_detect_executor();

    let mut schedules = load_schedules()?;

    // Both executors interpret the cron expression in LOCAL time: launchd's
    // StartCalendarInterval and crontab both do. Record the real IANA zone —
    // labelling every schedule "UTC" told users their `20 23 * * *` fired at
    // 23:20 UTC when it actually fires at 23:20 local, an 8-hour lie in +08:00.
    let timezone = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string());

    // Update existing or add new
    if let Some(existing) = schedules.iter_mut().find(|s| s.workflow == name) {
        existing.cron = cron.to_string();
        existing.enabled = true;
        existing.executor = executor.clone();
        existing.timezone = timezone;
    } else {
        schedules.push(Schedule {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: String::new(),
            workflow: name.to_string(),
            cron: cron.to_string(),
            timezone,
            enabled: true,
            notify: ScheduleNotify::default(),
            variables: Default::default(),
            on_missed: Default::default(),
            executor: executor.clone(),
        });
    }

    save_schedules(&schedules)?;
    println!("✅ Schedule set for '{}': {}", name, cron);

    // Only install system cron if Commander isn't handling it
    if executor == mur_common::schedule::ScheduleExecutor::SystemCron {
        // Install system-level scheduler (launchd on macOS, crontab on Linux)
        match crate::cmd::system_schedule::install(name, cron) {
            Ok(()) => {
                if cfg!(target_os = "macos") {
                    println!(
                        "   launchd plist installed at ~/Library/LaunchAgents/com.mur.schedule.{}.plist",
                        name
                    );
                } else {
                    println!("   crontab entry installed");
                }
            }
            Err(e) => {
                eprintln!(
                    "   ⚠️  Failed to install system schedule: {}. Workflow will only run when Commander is active.",
                    e
                );
            }
        }
    } else {
        println!("   Commander daemon is running — it will handle this schedule.");
    }
    Ok(())
}

pub(crate) fn cmd_schedule_remove(name: &str) -> Result<()> {
    let mut schedules = load_schedules()?;
    let before = schedules.len();
    schedules.retain(|s| s.workflow != name);

    if schedules.len() == before {
        println!("ℹ️  No schedule found for '{}'.", name);
        return Ok(());
    }

    save_schedules(&schedules)?;
    println!("🗑️  Schedule removed for '{}'.", name);

    // Remove system-level scheduler
    if let Err(e) = crate::cmd::system_schedule::remove(name) {
        eprintln!("   ⚠️  Failed to remove system schedule: {}", e);
    }
    Ok(())
}

pub(crate) fn cmd_schedule_enable(name: &str, enable: bool) -> Result<()> {
    let mut schedules = load_schedules()?;

    if let Some(entry) = schedules.iter_mut().find(|s| s.workflow == name) {
        entry.enabled = enable;
        save_schedules(&schedules)?;
        if enable {
            println!("▶️  Schedule enabled for '{}'.", name);
            if let Some(s) = schedules.iter().find(|s| s.workflow == name) {
                let _ = crate::cmd::system_schedule::install(name, &s.cron);
            }
        } else {
            println!("⏸️  Schedule disabled for '{}'.", name);
            let _ = crate::cmd::system_schedule::remove(name);
        }
    } else {
        println!(
            "ℹ️  No schedule found for '{}'. Use `mur workflow schedule set` first.",
            name
        );
    }

    Ok(())
}
