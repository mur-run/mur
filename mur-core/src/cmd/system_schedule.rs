//! System-level schedule integration (launchd on macOS, crontab on Linux).
//!
//! Installs/removes system scheduler entries so workflows run without Commander.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// The mur binary path (best-effort detection).
fn mur_binary() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "mur".to_string())
}

/// Install a system schedule for a workflow.
pub fn install(workflow_name: &str, cron_expr: &str) -> Result<()> {
    if cfg!(target_os = "macos") {
        install_launchd(workflow_name, cron_expr)
    } else {
        install_crontab(workflow_name, cron_expr)
    }
}

/// Remove a system schedule for a workflow.
pub fn remove(workflow_name: &str) -> Result<()> {
    if cfg!(target_os = "macos") {
        remove_launchd(workflow_name)
    } else {
        remove_crontab(workflow_name)
    }
}

/// List system-installed schedules (for display).
pub fn list_system_schedules() -> Vec<(String, String)> {
    if cfg!(target_os = "macos") {
        list_launchd()
    } else {
        list_crontab()
    }
}

// ── macOS launchd ──────────────────────────────────────────────────

fn plist_path(workflow_name: &str) -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join("Library/LaunchAgents")
        .join(format!("com.mur.schedule.{}.plist", workflow_name))
}

/// Convert a 5-field cron expression to launchd calendar interval.
/// Supports: minute, hour, day-of-month, month, day-of-week.
/// Wildcards (*) are omitted (launchd treats missing fields as "any").
fn cron_to_calendar_interval(cron_expr: &str) -> String {
    let parts: Vec<&str> = cron_expr.split_whitespace().collect();
    if parts.len() < 5 {
        // Fallback: run every hour
        return "    <dict>\n      <key>Minute</key>\n      <integer>0</integer>\n    </dict>"
            .to_string();
    }

    let mut entries = Vec::new();
    let fields = [
        ("Minute", parts[0]),
        ("Hour", parts[1]),
        ("Day", parts[2]),
        ("Month", parts[3]),
        ("Weekday", parts[4]),
    ];

    for (key, value) in &fields {
        if *value != "*"
            && let Ok(num) = value.parse::<i32>()
        {
            entries.push(format!(
                "      <key>{}</key>\n      <integer>{}</integer>",
                key, num
            ));
        }
    }

    if entries.is_empty() {
        // All wildcards = every minute, default to every hour
        entries.push("      <key>Minute</key>\n      <integer>0</integer>".to_string());
    }

    format!("    <dict>\n{}\n    </dict>", entries.join("\n"))
}

fn install_launchd(workflow_name: &str, cron_expr: &str) -> Result<()> {
    let path = plist_path(workflow_name);
    let label = format!("com.mur.schedule.{}", workflow_name);
    let mur = mur_binary();
    let calendar = cron_to_calendar_interval(cron_expr);
    let log_dir = dirs::home_dir().unwrap_or_default().join(".mur/logs");
    std::fs::create_dir_all(&log_dir)?;

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{mur}</string>
    <string>run</string>
    <string>{workflow_name}</string>
  </array>
  <key>StartCalendarInterval</key>
{calendar}
  <key>StandardOutPath</key>
  <string>{log_dir}/schedule-{workflow_name}.log</string>
  <key>StandardErrorPath</key>
  <string>{log_dir}/schedule-{workflow_name}.err</string>
  <key>RunAtLoad</key>
  <false/>
</dict>
</plist>"#,
        label = label,
        mur = mur,
        workflow_name = workflow_name,
        calendar = calendar,
        log_dir = log_dir.display(),
    );

    // Unload if already loaded
    let _ = std::process::Command::new("launchctl")
        .args(["unload", &path.to_string_lossy()])
        .output();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, plist)?;

    // Load the plist
    let status = std::process::Command::new("launchctl")
        .args(["load", &path.to_string_lossy()])
        .status()
        .context("Failed to run launchctl load")?;

    if !status.success() {
        eprintln!("⚠️  launchctl load returned non-zero, schedule may not be active");
    }

    Ok(())
}

fn remove_launchd(workflow_name: &str) -> Result<()> {
    let path = plist_path(workflow_name);
    if path.exists() {
        let _ = std::process::Command::new("launchctl")
            .args(["unload", &path.to_string_lossy()])
            .output();
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

fn list_launchd() -> Vec<(String, String)> {
    let agents_dir = dirs::home_dir()
        .unwrap_or_default()
        .join("Library/LaunchAgents");

    let mut results = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&agents_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("com.mur.schedule.") && name.ends_with(".plist") {
                let wf_name = name
                    .strip_prefix("com.mur.schedule.")
                    .and_then(|s| s.strip_suffix(".plist"))
                    .unwrap_or(&name)
                    .to_string();
                results.push((wf_name, "launchd".to_string()));
            }
        }
    }
    results
}

// ── Linux crontab ──────────────────────────────────────────────────

const CRON_TAG_PREFIX: &str = "# mur-schedule:";

fn install_crontab(workflow_name: &str, cron_expr: &str) -> Result<()> {
    let mur = mur_binary();
    let tag = format!("{}{}", CRON_TAG_PREFIX, workflow_name);
    let entry = format!(
        "{} {} run {} >> ~/.mur/logs/schedule-{}.log 2>&1 {}",
        cron_expr, mur, workflow_name, workflow_name, tag
    );

    // Read existing crontab
    let existing = std::process::Command::new("crontab")
        .arg("-l")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    // Remove old entry for this workflow (tag line + command line)
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|line| !line.contains(&tag))
        .map(|s| s.to_string())
        .collect();

    // Add new entry
    lines.push(tag);
    lines.push(entry);

    // Write back
    let new_crontab = lines.join("\n") + "\n";
    let mut child = std::process::Command::new("crontab")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("Failed to run crontab")?;

    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(new_crontab.as_bytes())?;
    child.wait()?;

    Ok(())
}

fn remove_crontab(workflow_name: &str) -> Result<()> {
    let tag = format!("{}{}", CRON_TAG_PREFIX, workflow_name);

    let existing = std::process::Command::new("crontab")
        .arg("-l")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    let lines: Vec<&str> = existing
        .lines()
        .filter(|line| !line.contains(&tag))
        .collect();

    let new_crontab = lines.join("\n") + "\n";
    let mut child = std::process::Command::new("crontab")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("Failed to run crontab")?;

    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(new_crontab.as_bytes())?;
    child.wait()?;

    Ok(())
}

fn list_crontab() -> Vec<(String, String)> {
    let existing = std::process::Command::new("crontab")
        .arg("-l")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    existing
        .lines()
        .filter_map(|line| {
            line.strip_prefix(CRON_TAG_PREFIX)
                .map(|name| (name.to_string(), "crontab".to_string()))
        })
        .collect()
}
