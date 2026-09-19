//! Clipboard images and completion, moved out of `mod.rs` for CLAUDE.md §4's
//! 800-line rule. Pure movement: verbatim.

use super::*;

/// Stage a base64 image (with its mime) to send with the next message.
pub(super) fn stage_image(app: &mut App, mime: &str, b64: String) {
    app.pending_image = Some((mime.to_string(), b64));
    app.push_system("📎 image attached — sent with your next message");
}

/// Grab a screenshot off the system clipboard and stage it; returns whether an
/// image was found. Used by Ctrl+V and by an empty bracketed paste (a Cmd+V of a
/// raw clipboard image on terminals that emit an empty paste event).
pub(super) fn attach_clipboard_image(app: &mut App) -> bool {
    match clipboard_png() {
        Some(b64) => {
            stage_image(app, "image/png", b64);
            true
        }
        None => false,
    }
}

/// Read a PNG off the macOS clipboard and return it base64-encoded.
///
/// ponytail: macOS-only via `osascript` (zero new deps — the clipboard already
/// carries a PNG flavor). Add `arboard` + an encoder for Linux/Windows if asked.
#[cfg(target_os = "macos")]
pub(super) fn clipboard_png() -> Option<String> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let tmp = std::env::temp_dir().join("mur-cli-paste.png");
    let path = tmp.to_str()?;
    // Dump the clipboard's PNG flavor to `path`; `the clipboard as «class PNGf»`
    // throws when there's no image, which the handler maps to "NOIMG".
    let script = format!(
        "try\n\
           set f to open for access (POSIX file \"{path}\") with write permission\n\
           set eof f to 0\n\
           write (the clipboard as «class PNGf») to f\n\
           close access f\n\
         on error\n\
           return \"NOIMG\"\n\
         end try"
    );
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .ok()?;
    if !out.status.success() || String::from_utf8_lossy(&out.stdout).contains("NOIMG") {
        return None;
    }
    let bytes = std::fs::read(&tmp).ok()?;
    let _ = std::fs::remove_file(&tmp);
    (!bytes.is_empty()).then(|| STANDARD.encode(&bytes))
}

#[cfg(not(target_os = "macos"))]
pub(super) fn clipboard_png() -> Option<String> {
    None
}

/// Recompute the completion menu from the current input. Called after every
/// edit and when Tab is pressed with the menu closed. `/` lines get the
/// command menu, `!` lines the shell menu, anything else none.
pub(super) fn refresh_completion(app: &mut App) {
    refresh_completion_with(app, false);
}

/// As `refresh_completion`, but `invited` says the user explicitly asked for
/// a menu (they pressed Tab).
///
/// The asymmetry is deliberate. `/` is murmur's own vocabulary — a closed set
/// the composer is happy to offer as you type. `!` hands the line to a shell,
/// where the last word is usually *finished*, not half-typed: `!mur agent
/// restart mur` inside murmur's own repo prefix-matched every `mur-*` crate
/// and popped a menu that then owned Enter, so the command ran as
/// `!mur agent restart mur-agent-gui/` (#002). Real shells never volunteer
/// path completion; they wait for Tab. So do we — once open, the menu keeps
/// re-filtering as you type, and Esc closes it for good.
pub(super) fn refresh_completion_with(app: &mut App, invited: bool) {
    let input = app.input_text();
    app.completion = if input.trim_start().starts_with('!') {
        if invited || app.completion.is_some() {
            shell_completion(app, input.trim_start())
        } else {
            None
        }
    } else {
        complete::compute(&input, &app.skills, &app.menu_ctx, &app.current_values())
    };
}

/// The shell menu for a `!` line: commands for the first word, paths after.
/// Never marks a `current` row — a path has no value in force.
pub(super) fn shell_completion(app: &mut App, line: &str) -> Option<complete::CompletionState> {
    let cwd = app
        .cwd
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let home = dirs::home_dir();
    let bins = app.path_bins().to_vec();
    let items = shell_complete::candidates(
        line,
        &shell_complete::ShellCompleteCtx {
            cwd: &cwd,
            path_bins: &bins,
            home: home.as_deref(),
        },
    );
    if items.is_empty() {
        return None;
    }
    Some(complete::CompletionState {
        items,
        selected: 0,
        spaced: false,
        current: None,
    })
}

/// Move the highlighted row by `delta`, wrapping.
pub(super) fn completion_move(app: &mut App, delta: isize) {
    if let Some(c) = &mut app.completion {
        let n = c.items.len() as isize;
        if n == 0 {
            return;
        }
        c.selected = (c.selected as isize + delta).rem_euclid(n) as usize;
    }
}

/// Accept the highlighted candidate: replace the input line with its insert
/// text. A command with a subcommand layer keeps the menu open (now showing
/// layer 2); everything else closes it.
pub(super) fn completion_accept(app: &mut App) {
    let Some(c) = app.completion.as_ref() else {
        return;
    };
    let Some(cand) = c.items.get(c.selected) else {
        app.completion = None;
        return;
    };
    let insert = cand.insert.clone();
    let descend = cand.has_children;
    app.set_input(&insert);
    if descend {
        // Descending into a directory is itself the invitation.
        refresh_completion_with(app, true);
    } else {
        app.completion = None;
    }
}
