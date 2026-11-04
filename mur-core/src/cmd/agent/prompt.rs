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

/// Heading printed between the authored prompt and the sections the runtime
/// appends, so a reader can tell the two apart at a glance.
pub const RUNTIME_INJECTED_HEADING: &str = "\n\n<!-- [runtime-injected] everything below is appended by the runtime at turn time; it is not in sys_prompt.md -->";

/// Heading printed before the per-turn rule templates, whose `{path}` is
/// only known once a turn is running.
pub const PER_TURN_TEMPLATES_HEADING: &str = "\n\n<!-- [runtime-injected, per turn] the blocks below are appended on every turn that has a session cwd; `{path}` is substituted at turn time -->";

/// `mur agent prompt show`. Default: the authored `sys_prompt.md` followed by
/// the static sections the runtime appends to every turn (output locations,
/// secrets names) and the per-turn rule templates with `{path}` left as a
/// placeholder — rendered by the same code the runtime uses. Before #2268
/// only the file was printed and the rules looked missing from every agent
/// but the concierge. `authored` keeps the byte-exact file for
/// `show > x.md && set -f x.md` round trips.
pub fn cmd_prompt_show(name: &str, authored: bool) -> Result<()> {
    let path = prompt_path_for(name)?;
    let body = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    // Print without an implicit trailing newline to keep show byte-exact.
    print!("{body}");
    if authored {
        return Ok(());
    }
    println!("{}", runtime_sections_for(name)?);
    Ok(())
}

/// The runtime-injected tail for `show`: heading, static sections, the
/// per-turn templates under their own heading, and the one-line note about
/// what is only known per turn.
fn runtime_sections_for(name: &str) -> Result<String> {
    use mur_agent_runtime::prompt_rules::{PER_TURN_NOTE, per_turn_templates, static_sections};
    let (_, profile) = super::load_profile_for_edit(name)?;
    let agent_home = resolve_mur_home()?.join("agents").join(name);
    // Same derivation as the runtime's grant; an underivable path omits the
    // scratch line there too, so the CLI must not invent one.
    let scratch = mur_agent_runtime::agent_paths::agent_scratch_dir(&agent_home).ok();
    let mut out = RUNTIME_INJECTED_HEADING.to_string();
    out.push_str(&static_sections(scratch.as_deref(), &profile.secrets));
    out.push_str(PER_TURN_TEMPLATES_HEADING);
    out.push_str(&per_turn_templates());
    out.push_str("\n\n");
    out.push_str(PER_TURN_NOTE);
    Ok(out)
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
