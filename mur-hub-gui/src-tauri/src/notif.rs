use mur_common::agent::QuietHours;
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NotifConfig {
    pub enabled: bool,
    pub daily_cap: u8,
    pub quiet_hours_enabled: bool,
    pub quiet_start: String,
    pub quiet_end: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NotifPatch {
    pub enabled: Option<bool>,
    pub daily_cap: Option<u8>,
    pub quiet_hours_enabled: Option<bool>,
    pub quiet_start: Option<String>,
    pub quiet_end: Option<String>,
}

pub(crate) fn get_notif_config_impl(home: &Path, name: &str) -> Result<NotifConfig, String> {
    let profile_path = home.join("agents").join(name).join("profile.yaml");
    let bytes = std::fs::read(&profile_path).map_err(|e| format!("read {e}"))?;
    let profile: mur_common::AgentProfile =
        serde_yaml_ng::from_slice(&bytes).map_err(|e| format!("parse: {e}"))?;
    let p = &profile.companion.proactive;
    Ok(config_from_proactive(p))
}

pub(crate) fn set_notif_config_impl(
    home: &Path,
    name: &str,
    patch: NotifPatch,
) -> Result<NotifConfig, String> {
    let profile_path = home.join("agents").join(name).join("profile.yaml");
    let bytes = std::fs::read(&profile_path).map_err(|e| format!("read {e}"))?;
    let mut profile: mur_common::AgentProfile =
        serde_yaml_ng::from_slice(&bytes).map_err(|e| format!("parse: {e}"))?;
    let p = &mut profile.companion.proactive;

    if let Some(v) = patch.enabled {
        p.enabled = v;
    }
    if let Some(v) = patch.daily_cap {
        p.daily_cap = v;
    }

    // Determine quiet_hours_enabled and start/end after applying patch
    let qh_enabled = patch
        .quiet_hours_enabled
        .unwrap_or_else(|| p.quiet_hours.is_some());
    let start = patch.quiet_start.clone().unwrap_or_else(|| {
        p.quiet_hours
            .as_ref()
            .map(|q| q.start.clone())
            .unwrap_or_default()
    });
    let end = patch.quiet_end.clone().unwrap_or_else(|| {
        p.quiet_hours
            .as_ref()
            .map(|q| q.end.clone())
            .unwrap_or_default()
    });

    p.quiet_hours = if qh_enabled {
        Some(QuietHours { start, end })
    } else {
        None
    };

    let yaml = serde_yaml_ng::to_string(&profile).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&profile_path, yaml).map_err(|e| format!("write {e}"))?;

    Ok(config_from_proactive(&profile.companion.proactive))
}

fn config_from_proactive(p: &mur_common::agent::ProactiveConfig) -> NotifConfig {
    let (quiet_hours_enabled, quiet_start, quiet_end) = match &p.quiet_hours {
        Some(q) => (true, q.start.clone(), q.end.clone()),
        None => (false, String::new(), String::new()),
    };
    NotifConfig {
        enabled: p.enabled,
        daily_cap: p.daily_cap,
        quiet_hours_enabled,
        quiet_start,
        quiet_end,
    }
}

#[tauri::command]
pub fn agent_get_notif_config(name: String) -> Result<NotifConfig, String> {
    get_notif_config_impl(&crate::mur_home_path(), &name)
}

#[tauri::command]
pub fn agent_set_notif_config(name: String, patch: NotifPatch) -> Result<NotifConfig, String> {
    set_notif_config_impl(&crate::mur_home_path(), &name, patch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_test_home(name: &str) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let agent_dir = dir.path().join("agents").join(name);
        fs::create_dir_all(&agent_dir).unwrap();
        let yaml = include_str!("../resources/mur-agent-template/profile.yaml")
            .replace("name: mur", &format!("name: {name}"))
            .replace(
                r#"id: "00000000-0000-0000-0000-000000000000""#,
                &format!(r#"id: "{name}""#),
            );
        fs::write(agent_dir.join("profile.yaml"), yaml).unwrap();
        let home = dir.path().to_path_buf();
        (dir, home)
    }

    #[test]
    fn round_trip_daily_cap() {
        let (_dir, home) = make_test_home("test-agent");
        let patch = NotifPatch {
            enabled: Some(true),
            daily_cap: Some(10),
            quiet_hours_enabled: None,
            quiet_start: None,
            quiet_end: None,
        };
        let cfg = set_notif_config_impl(&home, "test-agent", patch).unwrap();
        assert_eq!(cfg.daily_cap, 10);
        assert!(cfg.enabled);

        let read_back = get_notif_config_impl(&home, "test-agent").unwrap();
        assert_eq!(read_back.daily_cap, 10);
    }

    #[test]
    fn quiet_hours_toggle() {
        let (_dir, home) = make_test_home("qh-agent");
        let patch = NotifPatch {
            enabled: None,
            daily_cap: None,
            quiet_hours_enabled: Some(true),
            quiet_start: Some("23:00".to_string()),
            quiet_end: Some("07:00".to_string()),
        };
        let cfg = set_notif_config_impl(&home, "qh-agent", patch).unwrap();
        assert!(cfg.quiet_hours_enabled);
        assert_eq!(cfg.quiet_start, "23:00");
        assert_eq!(cfg.quiet_end, "07:00");

        let cfg2 = set_notif_config_impl(
            &home,
            "qh-agent",
            NotifPatch {
                quiet_hours_enabled: Some(false),
                enabled: None,
                daily_cap: None,
                quiet_start: None,
                quiet_end: None,
            },
        )
        .unwrap();
        assert!(!cfg2.quiet_hours_enabled);
    }
}
