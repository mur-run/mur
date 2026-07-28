//! Schedule claim/release — prevents double execution between CLI cron and Commander.
//!
//! The `executor` field in Schedule tracks who is responsible for ticking.
//! PID file at `~/.mur/commander/commander.pid` indicates Commander is alive.

use std::path::{Path, PathBuf};

use crate::schedule::{Schedule, ScheduleExecutor, SchedulesFile};

/// Default Commander PID file path.
pub fn commander_pid_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".mur")
        .join("commander")
        .join("commander.pid")
}

/// Check if Commander daemon is currently running.
pub fn is_commander_running() -> bool {
    is_commander_running_at(&commander_pid_path())
}

/// Check if Commander is running given a specific PID file path.
pub fn is_commander_running_at(pid_path: &Path) -> bool {
    let content = match std::fs::read_to_string(pid_path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let pid: u32 = match content.trim().parse() {
        Ok(p) => p,
        Err(_) => return false,
    };

    // Same liveness question as a runtime lock file — use the same answer
    // instead of a second hand-rolled probe. The old `#[cfg(not(unix))]` arm
    // returned false unconditionally, so on Windows `auto_detect_executor`
    // never chose Commander and `mur workflow` reclaimed schedules from a
    // Commander that was very much alive.
    crate::lock_file::pid_alive(pid)
}

/// Default schedules.yaml path.
pub fn schedules_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".mur")
        .join("schedules.yaml")
}

/// Load schedules from the default path.
pub fn load_schedules() -> Result<Vec<Schedule>, Box<dyn std::error::Error>> {
    let path = schedules_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)?;
    let file: SchedulesFile = serde_yaml::from_str(&content)?;
    Ok(file.schedules)
}

/// Save schedules to the default path.
pub fn save_schedules(schedules: &[Schedule]) -> Result<(), Box<dyn std::error::Error>> {
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

/// Claim all schedules for Commander — sets executor to Commander.
/// Returns the list of schedules that were claimed (had executor != Commander).
pub fn claim_all_for_commander() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut schedules = load_schedules()?;
    let mut claimed = Vec::new();

    for schedule in &mut schedules {
        if schedule.executor != ScheduleExecutor::Commander {
            claimed.push(schedule.workflow.clone());
            schedule.executor = ScheduleExecutor::Commander;
        }
    }

    if !claimed.is_empty() {
        save_schedules(&schedules)?;
    }

    Ok(claimed)
}

/// Release all schedules from Commander — sets executor back to SystemCron.
/// Returns the list of schedules that were released.
pub fn release_all_from_commander() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut schedules = load_schedules()?;
    let mut released = Vec::new();

    for schedule in &mut schedules {
        if schedule.executor == ScheduleExecutor::Commander {
            released.push(schedule.workflow.clone());
            schedule.executor = ScheduleExecutor::SystemCron;
        }
    }

    if !released.is_empty() {
        save_schedules(&schedules)?;
    }

    Ok(released)
}

/// Determine the appropriate executor for a new schedule.
/// If Commander is running, use Commander. Otherwise, use SystemCron.
pub fn auto_detect_executor() -> ScheduleExecutor {
    if is_commander_running() {
        ScheduleExecutor::Commander
    } else {
        ScheduleExecutor::SystemCron
    }
}
