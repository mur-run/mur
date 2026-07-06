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

#[derive(Debug, Clone)]
pub struct SystemSchedule {
    pub workflow: String,
    pub cron: Option<String>,
}

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

/// Extract cron expression from plist's StartCalendarInterval block.
/// Returns 5-field cron expression (minute hour day month weekday).
/// Single-integer fields become that value; missing fields become "*".
pub(crate) fn calendar_interval_to_cron(plist: &str) -> Option<String> {
    if !plist.contains("StartCalendarInterval") {
        return None;
    }
    let field = |key: &str| -> String {
        plist
            .split(&format!("<key>{key}</key>"))
            .nth(1)
            .and_then(|rest| rest.split("<integer>").nth(1))
            .and_then(|rest| rest.split("</integer>").next())
            .map(|v| v.trim().to_string())
            .unwrap_or_else(|| "*".to_string())
    };
    Some(format!(
        "{} {} {} {} {}",
        field("Minute"),
        field("Hour"),
        field("Day"),
        field("Month"),
        field("Weekday"),
    ))
}

/// Extract first five whitespace-separated fields from crontab line (cron expression).
pub(crate) fn crontab_line_to_cron(line: &str) -> Option<String> {
    let fields: Vec<&str> = line.split_whitespace().take(6).collect();
    if fields.len() < 5 {
        return None; // needs at least 5 schedule fields
    }
    Some(fields[..5].join(" "))
}

/// Detailed variant of list_system_schedules that recovers each entry's cron expression.
pub fn list_system_schedules_detailed() -> Vec<SystemSchedule> {
    if cfg!(target_os = "macos") {
        list_launchd()
            .into_iter()
            .map(|(workflow, _)| {
                let cron = std::fs::read_to_string(plist_path(&workflow))
                    .ok()
                    .and_then(|body| calendar_interval_to_cron(&body));
                SystemSchedule { workflow, cron }
            })
            .collect()
    } else {
        // Tagged crontab lines: "<5 cron fields> <cmd...> # mur-schedule:<name>"
        let output = std::process::Command::new("crontab").arg("-l").output();
        let Ok(out) = output else {
            return Vec::new();
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                let name = line.split(CRON_TAG_PREFIX).nth(1)?.trim().to_string();
                let cron = crontab_line_to_cron(line);
                Some(SystemSchedule {
                    workflow: name,
                    cron,
                })
            })
            .collect()
    }
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

#[cfg(test)]
mod p2_tests {
    use super::*;

    #[test]
    fn calendar_interval_round_trips_cron() {
        // What install_launchd writes for "30 9 * * 1-5" — only Minute/Hour/Weekday present.
        let plist = r#"<dict>
  <key>StartCalendarInterval</key>
    <dict>
      <key>Minute</key>
      <integer>30</integer>
      <key>Hour</key>
      <integer>9</integer>
      <key>Weekday</key>
      <integer>1</integer>
    </dict>
</dict>"#;
        assert_eq!(
            calendar_interval_to_cron(plist).as_deref(),
            Some("30 9 * * 1")
        );
    }

    #[test]
    fn calendar_interval_missing_block_is_none() {
        assert_eq!(calendar_interval_to_cron("<dict></dict>"), None);
    }

    #[test]
    fn crontab_line_extracts_first_five_fields() {
        let line = "0 9 * * * /opt/homebrew/bin/mur run daily >> ~/.mur/logs/x.log 2>&1 # mur-schedule:daily";
        assert_eq!(crontab_line_to_cron(line).as_deref(), Some("0 9 * * *"));
    }

    #[test]
    fn crontab_short_line_is_none() {
        assert_eq!(crontab_line_to_cron("0 9 *"), None);
    }
}
