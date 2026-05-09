//! `mur agent prompt` — show / edit / set the per-agent system prompt.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use super::{resolve_mur_home, write_atomic};

pub(crate) fn prompt_path_for(name: &str) -> Result<PathBuf> {
    let mur_home = resolve_mur_home()?;
    let dir = mur_home.join("agents").join(name);
    if !dir.exists() {
        bail!("agent '{name}' not found");
    }
    Ok(dir.join("sys_prompt.md"))
}

pub fn cmd_prompt_show(name: &str) -> Result<()> {
    let path = prompt_path_for(name)?;
    let body = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    // Print without an implicit trailing newline to keep show byte-exact.
    print!("{body}");
    Ok(())
}

pub fn cmd_prompt_edit(name: &str) -> Result<()> {
    let path = prompt_path_for(name)?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("spawn editor '{editor}'"))?;
    if !status.success() {
        bail!("editor exited with {status}");
    }
    Ok(())
}

pub fn cmd_prompt_set(name: &str, content: Option<&str>, file: Option<&str>) -> Result<()> {
    let path = prompt_path_for(name)?;
    let new_bytes: Vec<u8> = match (content, file) {
        (_, Some(p)) => fs::read(p).with_context(|| format!("read {p}"))?,
        (Some(s), None) => s.as_bytes().to_vec(),
        (None, None) => bail!("provide either inline CONTENT or -f FILE"),
    };

    // Preserve previous value as sys_prompt.md.bak before overwriting.
    if path.exists() {
        let bak = path.with_extension("md.bak");
        fs::copy(&path, &bak)
            .with_context(|| format!("backup {} -> {}", path.display(), bak.display()))?;
    }
    write_atomic(&path, &new_bytes)?;
    Ok(())
}
