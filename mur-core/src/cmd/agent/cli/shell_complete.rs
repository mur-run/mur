//! Completion for `!` lines in the composer: command names for the first
//! word (from a PATH scan the caller owns and caches), cwd-relative paths for
//! every later word. Pure over what it is handed, so the tests run on a
//! tempdir and a literal command list.
//!
//! ponytail: inserted paths are not quoted. A name with a space goes in as-is
//! and the user quotes it; escaping is the upgrade if that ever bites.

// Wired into the composer by the next commit; until then nothing in the
// binary calls it. Removed there.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use super::complete::{Candidate, MAX_MENU_ROWS};

pub struct ShellCompleteCtx<'a> {
    /// Directory relative paths resolve against (murmur's shell cwd).
    pub cwd: &'a Path,
    /// Executable names on `$PATH`, sorted, deduplicated.
    pub path_bins: &'a [String],
    /// What a leading `~` expands to. `None` leaves `~` alone.
    pub home: Option<&'a Path>,
}

/// Split the text after `!` into the head (kept verbatim, trailing space
/// included) and the last word (the one being completed).
fn split_last_word(body: &str) -> (&str, &str) {
    match body.rfind(' ') {
        Some(i) => (&body[..=i], &body[i + 1..]),
        None => ("", body),
    }
}

/// Candidates for the composer line `line`, which must start with `!`.
/// `insert` is the whole line with the last word replaced.
pub fn candidates(line: &str, ctx: &ShellCompleteCtx<'_>) -> Vec<Candidate> {
    if line.contains('\n') {
        return Vec::new();
    }
    let Some(body) = line.strip_prefix('!') else {
        return Vec::new();
    };
    let body = body.trim_start();
    let (head, word) = split_last_word(body);
    let mut out = if head.is_empty() {
        command_candidates(word, ctx.path_bins)
    } else {
        path_candidates(word, ctx)
    };
    out.truncate(MAX_MENU_ROWS);
    for c in &mut out {
        c.insert = format!("!{head}{}", c.insert);
    }
    out
}

/// First word: command names by prefix. An empty prefix offers nothing —
/// eight of two thousand commands is noise, not help.
fn command_candidates(word: &str, bins: &[String]) -> Vec<Candidate> {
    if word.is_empty() {
        return Vec::new();
    }
    bins.iter()
        .filter(|b| b.starts_with(word))
        .map(|b| Candidate {
            display: b.clone(),
            insert: format!("{b} "),
            desc: String::new(),
            has_children: false,
        })
        .collect()
}

/// A later word: entries of the word's directory, by file-name prefix.
/// Directories first with a trailing `/` and `has_children`, so accepting one
/// keeps the menu open on its contents.
fn path_candidates(word: &str, ctx: &ShellCompleteCtx<'_>) -> Vec<Candidate> {
    let (dir_part, name_part) = match word.rfind('/') {
        Some(i) => (&word[..=i], &word[i + 1..]),
        None => ("", word),
    };
    let dir_fs = resolve_dir(dir_part, ctx);
    let Ok(rd) = std::fs::read_dir(&dir_fs) else {
        return Vec::new();
    };
    let mut entries: Vec<(bool, String)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with(name_part) {
                return None;
            }
            if name.starts_with('.') && !name_part.starts_with('.') {
                return None;
            }
            // `path().is_dir()` follows symlinks; `file_type()` would not.
            Some((e.path().is_dir(), name))
        })
        .collect();
    // Directories first, then by name.
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    entries
        .into_iter()
        .map(|(is_dir, name)| Candidate {
            display: if is_dir {
                format!("{name}/")
            } else {
                name.clone()
            },
            insert: format!("{dir_part}{name}{}", if is_dir { "/" } else { " " }),
            desc: String::new(),
            has_children: is_dir,
        })
        .collect()
}

/// The directory `dir_part` (`""`, `src/`, `~/x/`, `/abs/`) names on disk.
fn resolve_dir(dir_part: &str, ctx: &ShellCompleteCtx<'_>) -> PathBuf {
    if dir_part.is_empty() {
        return ctx.cwd.to_path_buf();
    }
    let expanded: PathBuf = match (dir_part.strip_prefix("~/"), ctx.home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(dir_part),
    };
    if expanded.is_absolute() {
        expanded
    } else {
        ctx.cwd.join(expanded)
    }
}

/// Every executable file name on `$PATH`, sorted and deduplicated. Scanned
/// once per session by the caller; a new install needs a new murmur, the
/// same as a new shell.
pub fn scan_path_bins() -> Vec<String> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let mut set = std::collections::BTreeSet::new();
    for dir in std::env::split_paths(&path) {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if is_executable_file(&p) {
                set.insert(e.file_name().to_string_lossy().into_owned());
            }
        }
    }
    set.into_iter().collect()
}

#[cfg(unix)]
fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn is_executable_file(p: &Path) -> bool {
    p.is_file()
        && p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "exe" | "cmd" | "bat" | "com"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tempdir with: dirs `docs/`, `src/` (holding `main.rs`); files
    /// `Cargo.toml`, `README.md`, `.hidden`.
    fn tree() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("docs")).unwrap();
        std::fs::create_dir(t.path().join("src")).unwrap();
        std::fs::write(t.path().join("src/main.rs"), "").unwrap();
        std::fs::write(t.path().join("Cargo.toml"), "").unwrap();
        std::fs::write(t.path().join("README.md"), "").unwrap();
        std::fs::write(t.path().join(".hidden"), "").unwrap();
        t
    }

    fn bins() -> Vec<String> {
        ["cargo", "cat", "ls"].map(String::from).to_vec()
    }

    fn ctx<'a>(t: &'a tempfile::TempDir, bins: &'a [String]) -> ShellCompleteCtx<'a> {
        ShellCompleteCtx {
            cwd: t.path(),
            path_bins: bins,
            home: Some(t.path()),
        }
    }

    fn inserts(v: &[Candidate]) -> Vec<String> {
        v.iter().map(|c| c.insert.clone()).collect()
    }

    #[test]
    fn first_word_completes_command_names_by_prefix() {
        let t = tree();
        let b = bins();
        assert_eq!(
            inserts(&candidates("!ca", &ctx(&t, &b))),
            ["!cargo ", "!cat "]
        );
        assert!(
            candidates("!", &ctx(&t, &b)).is_empty(),
            "no prefix, no list"
        );
        assert!(candidates("!zz", &ctx(&t, &b)).is_empty());
    }

    #[test]
    fn later_words_list_the_cwd_directories_first_with_a_slash() {
        let t = tree();
        let b = bins();
        let out = candidates("!ls ", &ctx(&t, &b));
        assert_eq!(
            inserts(&out),
            ["!ls docs/", "!ls src/", "!ls Cargo.toml ", "!ls README.md "]
        );
        assert!(out[0].has_children && !out[2].has_children);
        assert_eq!(out[0].display, "docs/");
    }

    #[test]
    fn hidden_entries_only_when_the_prefix_starts_with_a_dot() {
        let t = tree();
        let b = bins();
        assert!(
            candidates("!ls ", &ctx(&t, &b))
                .iter()
                .all(|c| !c.display.starts_with('.'))
        );
        assert_eq!(
            inserts(&candidates("!ls .", &ctx(&t, &b))),
            ["!ls .hidden "]
        );
    }

    #[test]
    fn descends_into_a_directory_and_keeps_the_head() {
        let t = tree();
        let b = bins();
        assert_eq!(
            inserts(&candidates("!cat src/", &ctx(&t, &b))),
            ["!cat src/main.rs "]
        );
        assert_eq!(
            inserts(&candidates("!cat -n src/m", &ctx(&t, &b))),
            ["!cat -n src/main.rs "]
        );
    }

    #[test]
    fn tilde_expands_to_home() {
        let t = tree();
        let b = bins();
        assert_eq!(
            inserts(&candidates("!ls ~/d", &ctx(&t, &b))),
            ["!ls ~/docs/"]
        );
    }

    #[test]
    fn an_absolute_directory_is_used_as_is() {
        let t = tree();
        let b = bins();
        let abs = format!("!ls {}/s", t.path().display());
        let out = candidates(&abs, &ctx(&t, &b));
        assert_eq!(out.len(), 1);
        assert!(out[0].insert.ends_with("/src/"), "{}", out[0].insert);
    }

    #[test]
    fn the_list_is_capped_and_multiline_input_has_none() {
        let t = tree();
        let b = bins();
        for i in 0..12 {
            std::fs::write(t.path().join(format!("f{i:02}")), "").unwrap();
        }
        assert_eq!(candidates("!ls f", &ctx(&t, &b)).len(), MAX_MENU_ROWS);
        assert!(candidates("!ls\nf", &ctx(&t, &b)).is_empty());
        assert!(candidates("ls f", &ctx(&t, &b)).is_empty(), "no leading !");
    }

    #[test]
    fn scan_path_bins_reads_the_env_path() {
        let t = tempfile::tempdir().unwrap();
        let exe = t.path().join("mytool");
        std::fs::write(&exe, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(t.path().join("notes.txt"), "").unwrap();
        // Process-wide env: this test owns PATH for its duration.
        let saved = std::env::var_os("PATH");
        unsafe { std::env::set_var("PATH", t.path()) };
        let bins = scan_path_bins();
        unsafe {
            match saved {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }
        #[cfg(unix)]
        assert_eq!(bins, ["mytool"]);
        #[cfg(windows)]
        assert!(bins.is_empty(), "no .exe in the dir");
    }
}
