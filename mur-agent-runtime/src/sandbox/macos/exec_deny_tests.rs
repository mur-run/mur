use super::*;
use std::path::PathBuf;

fn policy_with(write: Vec<PathBuf>, deny: Vec<PathBuf>) -> SandboxPolicy {
    SandboxPolicy {
        fs_write: write,
        fs_deny: deny,
        ..Default::default()
    }
}

/// #2317: a `mur` CLI inside a granted exec directory is closed again by a
/// literal deny emitted AFTER the directory allow, so last-match-wins keeps it
/// shut — and `exec_permitted` (what `mur agent doctor` predicts) agrees.
#[test]
fn the_mur_cli_in_an_exec_dir_is_denied_after_the_dir_allow() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = std::fs::canonicalize(tmp.path()).unwrap().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for f in ["mur", "jq"] {
        std::fs::write(bin.join(f), b"x").unwrap();
    }
    let mut policy = policy_with(vec![], vec![]);
    policy.spawn_mode = SpawnMode::Allowlist;
    policy.fs_exec = vec![bin.clone()];
    let sbpl = build_sbpl_profile(&policy);

    let allow = format!("(allow process-exec* (subpath \"{}\"))", bin.display());
    let deny = format!(
        "(deny process-exec* (path-literal \"{}\"))",
        bin.join("mur").display()
    );
    let (a, d) = (sbpl.find(&allow), sbpl.find(&deny));
    assert!(a.is_some() && d.is_some(), "missing allow or deny:\n{sbpl}");
    assert!(d > a, "the deny must follow the directory allow:\n{sbpl}");
    assert!(!exec_permitted(&policy, &bin.join("mur")));
    // Negative control: a sibling in the same directory stays exec-able.
    assert!(exec_permitted(&policy, &bin.join("jq")));

    // An explicit allowlist entry is consent: no deny, and doctor agrees.
    policy.spawn_allowed_paths = vec![bin.join("mur")];
    let sbpl = build_sbpl_profile(&policy);
    assert!(
        !sbpl.contains(&deny),
        "allowlisted CLI must not be denied:\n{sbpl}"
    );
    assert!(exec_permitted(&policy, &bin.join("mur")));
}
