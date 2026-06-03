//! `mur agent export` — Hub side. Produces a `.muragent` from an installed
//! agent using the exact CLI path (template/sanitized). Spec §6 (C).

use std::path::Path;

/// Validate that `out_path` is safe to write a `.muragent` file to:
/// - must have a `.muragent` extension
/// - must not contain path traversal components (`..`)
fn validate_out_path(out_path: &str) -> Result<(), String> {
    let path = Path::new(out_path);
    match path.extension().and_then(|e| e.to_str()) {
        Some("muragent") => {}
        _ => return Err(format!("out_path must have a .muragent extension: {out_path}")),
    }
    for component in path.components() {
        use std::path::Component;
        if matches!(component, Component::ParentDir) {
            return Err(format!(
                "out_path contains path traversal component (..): {out_path}"
            ));
        }
    }
    Ok(())
}

#[tauri::command]
pub fn export_muragent_file(name: String, out_path: String) -> Result<String, String> {
    validate_out_path(&out_path)?;
    mur_core::cmd::agent::export::export_agent_to_muragent(&name, Path::new(&out_path))
        .map_err(|e| e.to_string())?;
    Ok(out_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_path_accepted() {
        assert!(validate_out_path("/tmp/my-agent.muragent").is_ok());
    }

    #[test]
    fn wrong_extension_rejected() {
        assert!(validate_out_path("/tmp/agent.zip").is_err());
        assert!(validate_out_path("/tmp/agent").is_err());
    }

    #[test]
    fn traversal_rejected() {
        assert!(validate_out_path("../etc/passwd.muragent").is_err());
        assert!(validate_out_path("/tmp/../etc/evil.muragent").is_err());
    }
}
