//! Light YAML mutation helpers — atomic write and a line-aware scalar
//! replacement that preserves surrounding comments.
//!
//! `serde_yaml_ng::Value` round-trips drop comments, so for hand-edited
//! profile.yaml files we use a text-level mutator for the common case
//! ("set this top-level scalar to that value"). Anything more complex
//! (nested keys, list mutations) goes through the typed AgentProfile
//! editor in `cmd::agent` and accepts the comment loss as a tradeoff.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;

/// Atomically write `bytes` to `path` via temp + rename. The temp file is
/// `<path>.<ext>.tmp` and is replaced into place on success; the original
/// file is left untouched on any earlier error.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Replace the scalar value of a top-level key in YAML text, preserving the
/// rest of the file (other lines, leading comments, inline trailing comments,
/// blank lines, line endings). Returns Err if the key is not found at the
/// top level.
pub fn set_top_level_scalar(yaml: &str, key: &str, new_value: &str) -> Result<String> {
    let mut out = String::with_capacity(yaml.len() + new_value.len());
    let mut hit = false;
    for line in yaml.split_inclusive('\n') {
        // Match top-level key (no leading indent) followed by a colon. We
        // intentionally ignore lines inside nested mappings.
        let line_no_eol = line.trim_end_matches('\n').trim_end_matches('\r');
        if !hit
            && !line.starts_with(|c: char| c.is_whitespace())
            && let Some(after) = line_no_eol.strip_prefix(&format!("{key}:"))
        {
            // Preserve a trailing inline comment if present.
            let after_trim = after.trim_start();
            let inline_comment = match after_trim.find('#') {
                Some(idx) => after_trim[idx..].to_string(),
                None => String::new(),
            };
            let line_end = if line.ends_with("\r\n") {
                "\r\n"
            } else if line.ends_with('\n') {
                "\n"
            } else {
                ""
            };
            let suffix = if inline_comment.is_empty() {
                String::new()
            } else {
                format!("  {inline_comment}")
            };
            out.push_str(&format!("{key}: {new_value}{suffix}{line_end}"));
            hit = true;
        } else {
            out.push_str(line);
        }
    }
    if !hit {
        bail!("key '{key}' not found at top level");
    }
    Ok(out)
}
