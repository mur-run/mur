use std::path::Path;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MemoryView {
    pub relationship: String,
    pub formality: String,
    pub first_memory: String,
    pub sys_prompt: String,
    pub companion_initialised: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MemoryPatch {
    pub relationship: Option<String>,
    pub formality: Option<String>,
    pub first_memory: Option<String>,
    pub sys_prompt: Option<String>,
}

pub fn get_memory_impl(home: &Path, name: &str) -> Result<MemoryView, String> {
    let rel_path = home
        .join("agents")
        .join(name)
        .join("companion")
        .join("relationship.json");

    let (relationship, formality, first_memory, companion_initialised) = if rel_path.exists() {
        let bytes = std::fs::read(&rel_path)
            .map_err(|e| format!("read relationship.json: {e}"))?;
        let v: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("parse relationship.json: {e}"))?;

        let relationship = v["relationship"]
            .as_str()
            .unwrap_or("friend")
            .to_string();
        let formality = v["formality"]
            .as_str()
            .unwrap_or("neutral")
            .to_string();
        let first_memory = v["first_memory"]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();

        (relationship, formality, first_memory, true)
    } else {
        ("friend".to_string(), "neutral".to_string(), String::new(), false)
    };

    let sys_prompt = std::fs::read_to_string(
        home.join("agents").join(name).join("sys_prompt.md"),
    )
    .unwrap_or_default();

    Ok(MemoryView {
        relationship,
        formality,
        first_memory,
        sys_prompt,
        companion_initialised,
    })
}

pub fn set_memory_impl(
    home: &Path,
    name: &str,
    patch: MemoryPatch,
) -> Result<MemoryView, String> {
    let rel_path = home
        .join("agents")
        .join(name)
        .join("companion")
        .join("relationship.json");

    if rel_path.exists()
        && (patch.relationship.is_some()
            || patch.formality.is_some()
            || patch.first_memory.is_some())
    {
        let bytes = std::fs::read(&rel_path)
            .map_err(|e| format!("read relationship.json: {e}"))?;
        let mut v: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("parse relationship.json: {e}"))?;

        if let Some(rel) = patch.relationship {
            v["relationship"] = serde_json::Value::String(rel);
        }
        if let Some(fmt) = patch.formality {
            v["formality"] = serde_json::Value::String(fmt);
        }
        if let Some(text) = patch.first_memory {
            if v["first_memory"].is_object() {
                v["first_memory"]["text"] = serde_json::Value::String(text);
            } else {
                let now = chrono::Utc::now().to_rfc3339();
                v["first_memory"] = serde_json::json!({
                    "text": text,
                    "established_at": now,
                });
            }
        }

        let updated =
            serde_json::to_string_pretty(&v).map_err(|e| format!("serialize: {e}"))?;
        std::fs::write(&rel_path, updated)
            .map_err(|e| format!("write relationship.json: {e}"))?;
    }

    if let Some(prompt) = patch.sys_prompt {
        let sp_path = home.join("agents").join(name).join("sys_prompt.md");
        std::fs::write(&sp_path, &prompt)
            .map_err(|e| format!("write sys_prompt.md: {e}"))?;
    }

    get_memory_impl(home, name)
}

pub fn reset_sys_prompt_impl(home: &Path, name: &str) -> Result<String, String> {
    let content = format!("# {name}\n\nYou are an assistant.\n");
    let sp_path = home.join("agents").join(name).join("sys_prompt.md");
    std::fs::write(&sp_path, &content)
        .map_err(|e| format!("write sys_prompt.md: {e}"))?;
    Ok(content)
}

#[tauri::command]
pub fn agent_get_memory(name: String) -> Result<MemoryView, String> {
    get_memory_impl(&crate::mur_home_path(), &name)
}

#[tauri::command]
pub fn agent_set_memory(name: String, patch: MemoryPatch) -> Result<MemoryView, String> {
    set_memory_impl(&crate::mur_home_path(), &name, patch)
}

#[tauri::command]
pub fn agent_reset_sys_prompt(name: String) -> Result<String, String> {
    reset_sys_prompt_impl(&crate::mur_home_path(), &name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_agent(name: &str) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let agent_dir = dir.path().join("agents").join(name);
        fs::create_dir_all(agent_dir.join("companion")).unwrap();
        fs::write(
            agent_dir.join("sys_prompt.md"),
            format!("# {name}\n\nYou are an assistant.\n"),
        )
        .unwrap();
        let home = dir.path().to_path_buf();
        (dir, home)
    }

    #[test]
    fn get_memory_no_relationship_file() {
        let (_dir, home) = make_agent("solo");
        let view = get_memory_impl(&home, "solo").unwrap();
        assert!(!view.companion_initialised);
        assert_eq!(view.relationship, "friend");
        assert!(view.sys_prompt.contains("solo"));
    }

    #[test]
    fn get_memory_with_relationship_file() {
        let (dir, home) = make_agent("buddy");
        let rp = dir.path().join("agents/buddy/companion/relationship.json");
        fs::write(
            &rp,
            r#"{"version":1,"relationship":"coach","formality":"formal","first_memory":{"text":"prefers short answers","established_at":"2026-01-01T00:00:00Z"}}"#,
        )
        .unwrap();
        let view = get_memory_impl(&home, "buddy").unwrap();
        assert!(view.companion_initialised);
        assert_eq!(view.relationship, "coach");
        assert_eq!(view.formality, "formal");
        assert_eq!(view.first_memory, "prefers short answers");
    }

    #[test]
    fn set_memory_round_trip() {
        let (dir, home) = make_agent("rtagent");
        let rp = dir.path().join("agents/rtagent/companion/relationship.json");
        fs::write(&rp, r#"{"version":1,"relationship":"friend","formality":"neutral"}"#).unwrap();
        let patch = MemoryPatch {
            relationship: Some("mentor".to_string()),
            formality: None,
            first_memory: Some("loves Rust".to_string()),
            sys_prompt: None,
        };
        let view = set_memory_impl(&home, "rtagent", patch).unwrap();
        assert_eq!(view.relationship, "mentor");
        assert_eq!(view.first_memory, "loves Rust");
    }

    #[test]
    fn reset_sys_prompt_restores_default() {
        let (_dir, home) = make_agent("reset-me");
        let path = home.join("agents/reset-me/sys_prompt.md");
        fs::write(&path, "# Custom\n\nCustom prompt.").unwrap();
        let result = reset_sys_prompt_impl(&home, "reset-me").unwrap();
        assert!(result.contains("reset-me"));
        assert!(result.contains("You are an assistant"));
        let on_disk = fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, result);
    }
}
