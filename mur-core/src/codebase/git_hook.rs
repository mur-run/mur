use super::*;

/// Marker line identifying the mur block inside `post-commit`.
pub(super) const HOOK_MARKER: &str = "# mur auto-index";
/// Bump when the block changes. A block without this exact line is rewritten
/// in place on the next index run, which is how already-installed hooks pick
/// up the new form without anyone touching them by hand.
const HOOK_VERSION_LINE: &str = "# hook-version: 2";

fn hook_block() -> String {
    // No path baked in: `--main-repo` makes mur resolve the primary repo from
    // cwd (following linked worktrees), so renaming the directory cannot strand
    // the hook and every repo carries an identical block. mur itself is resolved
    // at hook-run time from PATH (brew, cargo, ...), falling back to the
    // ~/.mur/bin installer location.
    format!(
        "\n{HOOK_MARKER}\n{HOOK_VERSION_LINE}\nMUR_BIN=\"$(command -v mur || true)\"\n[ -z \"$MUR_BIN\" ] && [ -x \"$HOME/.mur/bin/mur\" ] && MUR_BIN=\"$HOME/.mur/bin/mur\"\nif [ -n \"$MUR_BIN\" ]; then\n  \"$MUR_BIN\" project index --main-repo --quiet --background\nfi\n"
    )
}

/// Byte span of an existing mur block: from the marker line through the end of
/// the `fi` line that closes it. `None` when the hook has no mur block.
fn hook_block_span(existing: &str) -> Option<(usize, usize)> {
    let start = existing.find(HOOK_MARKER)?;
    let mut from = start;
    loop {
        let i = existing[from..].find("\nfi")?;
        let end = from + i + "\nfi".len();
        // Accept only a bare `fi` line, not a longer word that starts with it.
        match existing[end..].chars().next() {
            None => return Some((start, end)),
            Some('\n') => return Some((start, end + 1)),
            Some(_) => from = end,
        }
    }
}

pub fn ensure_git_hook(project_path: &Path, quiet: bool) -> Result<bool> {
    // #1672: honour core.hooksPath, worktrees and submodules — ask git.
    let hooks_dir = match hooks_dir::resolve(project_path) {
        hooks_dir::HooksDir::Writable(dir) => dir,
        hooks_dir::HooksDir::None => return Ok(false),
        hooks_dir::HooksDir::InWorkTree(dir) => {
            // A versioned hooks dir is the user's to edit. The caller reports
            // it to the user (cmd/project hook_report), so only trace here.
            tracing::debug!(dir = %dir.display(), "auto-index hook not installed: core.hooksPath is in the working tree");
            return Ok(false);
        }
    };
    let hook_path = hooks_dir.join("post-commit");
    let existing = std::fs::read_to_string(&hook_path).unwrap_or_default();

    let (body, action) = match hook_block_span(&existing) {
        Some(_) if existing.contains(HOOK_VERSION_LINE) => return Ok(false),
        // Older block (pre-v2 baked in an absolute --path): swap it out in place,
        // leaving whatever the user keeps before or after it untouched.
        Some((start, end)) => {
            let head = existing[..start]
                .strip_suffix('\n')
                .unwrap_or(&existing[..start]);
            (
                format!("{head}{}{}", hook_block(), &existing[end..]),
                "upgraded",
            )
        }
        None if existing.is_empty() => (format!("#!/bin/sh\n{}", hook_block()), "installed"),
        None => (format!("{existing}{}", hook_block()), "installed"),
    };
    std::fs::write(&hook_path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))?;
    }
    if !quiet {
        eprintln!("  Git hook {action} for auto-reindex on commit");
    }
    Ok(true)
}
