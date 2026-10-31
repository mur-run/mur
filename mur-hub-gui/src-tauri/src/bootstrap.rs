//! Process-level bootstrap helpers: `MUR_HOME` resolution, the `host_path` handshake
//! file, tracing init, and the `--regenerate-stub` display-name lookup.

use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

pub(crate) fn mur_home_path() -> PathBuf {
    std::env::var("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".mur")
        })
}

/// Atomically write `~/.mur/host_path` so per-agent launcher stubs can locate
/// the Hub binary and detect version mismatches (spec §5.2).
///
/// Format: two lines — `<absolute_exe_path>\n<version>`.
pub(crate) fn write_host_path(version: &str, mur_home: &std::path::Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?
        .canonicalize()
        .unwrap_or_else(|_| std::env::current_exe().unwrap());
    std::fs::create_dir_all(mur_home).map_err(|e| anyhow::anyhow!("create mur_home: {e}"))?;
    let tmp = mur_home.join("host_path.tmp");
    std::fs::write(&tmp, format!("{}\n{}", exe.display(), version))
        .map_err(|e| anyhow::anyhow!("write host_path.tmp: {e}"))?;
    std::fs::rename(&tmp, mur_home.join("host_path"))
        .map_err(|e| anyhow::anyhow!("rename host_path: {e}"))?;
    Ok(())
}

pub(crate) fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init();
}

pub(crate) fn load_display_name_for_stub(agent_dir: &std::path::Path) -> Option<String> {
    let yaml = std::fs::read_to_string(agent_dir.join("profile.yaml")).ok()?;
    for line in yaml.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("name:") {
            let name = rest.trim().trim_matches('"').trim_matches('\'');
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn lib_links() {
        let _ = super::init_tracing;
        let _ = super::mur_home_path;
    }
}
