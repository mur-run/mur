//! Source scans for the PR's structural rules (Hub spec §Tests): the Hub
//! holds no copy of the orchestrator's file logic, only `dialog.rs` builds
//! `Issuer::Hub`, the proposal chip's restart never touches an override, and
//! the override module never routes a dialog through the plugin that remaps
//! `Cancel` (revision 10).

use std::path::{Path, PathBuf};

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs")
            && !p
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("_tests.rs")
        {
            out.push(p);
        }
    }
}

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn all_sources() -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    sources(&src_root(), &mut files);
    files
        .into_iter()
        .map(|p| {
            let s = std::fs::read_to_string(&p).unwrap();
            (p, s)
        })
        .collect()
}

/// Built at runtime so this file's own text never matches.
fn needle(parts: &[&str]) -> String {
    parts.concat()
}

/// `needle` as a whole identifier: the next char is not part of a name, so
/// `fn classify(` matches and the Hub's unrelated `fn classify_remote_provider`
/// does not.
fn has_word(s: &str, needle: &str) -> bool {
    s.match_indices(needle).any(|(i, _)| {
        !s[i + needle.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

#[test]
fn no_copy_of_the_orchestrator_lives_in_the_hub() {
    // Type and constant copies are banned anywhere in the crate.
    let anywhere = [
        needle(&["override_", "max_age"]),
        needle(&["enum ", "IssuedBy"]),
        needle(&["enum ", "ConfirmedBy"]),
        needle(&["fn ", "is_valid_pair"]),
    ];
    for (p, s) in all_sources() {
        for b in &anywhere {
            assert!(!has_word(&s, b), "{} contains {b:?}", p.display());
        }
    }
    // The file operations are banned in the override module, where a copy
    // would live. (`dashboard.rs` has an unrelated `fn classify` for HTTP
    // probes.)
    let file_ops = [
        needle(&["fn ", "sweep"]),
        needle(&["fn ", "publish"]),
        needle(&["fn ", "withdraw"]),
        needle(&["fn ", "classify"]),
    ];
    let mut files = Vec::new();
    sources(&src_root().join("override_restart"), &mut files);
    for p in files {
        let s = std::fs::read_to_string(&p).unwrap();
        for b in &file_ops {
            assert!(!has_word(&s, b), "{} contains {b:?}", p.display());
        }
    }
}

#[test]
fn has_word_respects_identifier_boundaries() {
    assert!(has_word("fn classify(x)", "fn classify"));
    assert!(!has_word("fn classify_remote(x)", "fn classify"));
}

#[test]
fn only_dialog_rs_builds_the_hub_issuer() {
    let hub = needle(&["Issuer::", "Hub"]);
    let allowed = src_root().join("override_restart").join("dialog.rs");
    for (p, s) in all_sources() {
        if p != allowed {
            assert!(!s.contains(hub.as_str()), "{} names {hub}", p.display());
        }
    }
    let dialog = std::fs::read_to_string(&allowed).unwrap();
    assert!(dialog.contains(hub.as_str()));
}

#[test]
fn the_override_module_never_uses_the_dialog_plugin() {
    let plugin = needle(&["tauri_plugin", "_dialog"]);
    let mut files = Vec::new();
    sources(&src_root().join("override_restart"), &mut files);
    assert!(!files.is_empty());
    for p in files {
        let s = std::fs::read_to_string(&p).unwrap();
        // The module docs name the plugin to explain why it is avoided; only
        // a path use (`tauri_plugin_dialog::`) counts.
        let path_use = format!("{plugin}::");
        assert!(!s.contains(&path_use), "{} uses {plugin}", p.display());
    }
}

#[test]
fn the_chip_restart_never_touches_an_override() {
    let s = std::fs::read_to_string(src_root().join("proposal_chip.rs")).unwrap();
    let start = s
        .find("pub async fn restart_agent(")
        .expect("restart_agent exists");
    let body = &s[start..];
    let end = body.find("\n}\n").expect("end of restart_agent");
    let body = &body[..end];
    for w in [
        "no_hitl",
        "net_unrestricted",
        "full_perms",
        "override_restart::run",
        "OverrideFlags",
    ] {
        assert!(!body.contains(w), "restart_agent mentions {w}");
    }
}
