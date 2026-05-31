// Windows: gated — drives the `mur agent create/export/install` CLI which
// spawns the runtime and depends on unix-style symlink + process pipe
// semantics (same rationale as agent_export.rs).
#![cfg(unix)]

//! End-to-end "give-to-a-friend" round trip for the Export UX Phase B wizard:
//!
//!   create agent  →  `mur agent export … .muragent`  →  install into a *fresh*
//!   MUR_HOME with `mur agent install … --model <ref>`  →  the agent's profile
//!   is bound to the registry ref non-interactively.
//!
//! Covers the non-interactive `--model` surface added in Phase B (spec §7.5):
//! a server / script installs a shared `.muragent` and pins its model without
//! any TTY prompt.

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn mur() -> &'static str {
    env!("CARGO_BIN_EXE_mur")
}

fn run(home: &Path, bin_dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(mur())
        .env("MUR_HOME", home)
        .env("MUR_AGENT_BIN_DIR", bin_dir)
        .env("MUR_AGENT_RUNTIME_BIN", "/tmp/runtime-stub")
        .args(args)
        .output()
        .expect("spawn mur")
}

fn assert_ok(out: &std::process::Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn muragent_install_with_model_flag_binds_profile() {
    // ── Source machine: create + export a portable .muragent ──
    let src_home = TempDir::new().unwrap();
    let src_bin = TempDir::new().unwrap();
    assert_ok(
        &run(
            src_home.path(),
            src_bin.path(),
            &["agent", "create", "demo", "--no-interactive"],
        ),
        "create",
    );

    let out_dir = TempDir::new().unwrap();
    let pkg = out_dir.path().join("demo.muragent");
    assert_ok(
        &run(
            src_home.path(),
            src_bin.path(),
            &[
                "agent",
                "export",
                "demo",
                "--out",
                pkg.to_str().unwrap(),
                // muragent is the default format, but be explicit for the test.
                "--format",
                "muragent",
            ],
        ),
        "export",
    );
    assert!(pkg.exists(), ".muragent package should exist");

    // ── Friend's machine: fresh home, register a model, install with --model ──
    let dst_home = TempDir::new().unwrap();
    let dst_bin = TempDir::new().unwrap();

    // The --model ref must already exist in the *target* registry.
    assert_ok(
        &run(
            dst_home.path(),
            dst_bin.path(),
            &[
                "model",
                "add",
                "ollama_llama3_2_3b",
                "--provider",
                "ollama",
                "--model",
                "llama3.2:3b",
            ],
        ),
        "model add",
    );

    let install = run(
        dst_home.path(),
        dst_bin.path(),
        &[
            "agent",
            "install",
            pkg.to_str().unwrap(),
            "--model",
            "ollama_llama3_2_3b",
        ],
    );
    assert_ok(&install, "install --model");

    // ── The installed agent's profile is bound to the ref, no prompt needed ──
    let profile_path = dst_home.path().join("agents").join("demo").join("profile.yaml");
    assert!(
        profile_path.exists(),
        "installed profile should exist at {}",
        profile_path.display()
    );
    let profile = std::fs::read_to_string(&profile_path).unwrap();
    assert!(
        profile.contains("model_ref: ollama_llama3_2_3b"),
        "profile should be bound to the --model ref, got:\n{profile}"
    );
}

#[test]
fn muragent_install_with_unknown_model_ref_fails() {
    // Export from a throwaway source home.
    let src_home = TempDir::new().unwrap();
    let src_bin = TempDir::new().unwrap();
    assert_ok(
        &run(
            src_home.path(),
            src_bin.path(),
            &["agent", "create", "demo", "--no-interactive"],
        ),
        "create",
    );
    let out_dir = TempDir::new().unwrap();
    let pkg = out_dir.path().join("demo.muragent");
    assert_ok(
        &run(
            src_home.path(),
            src_bin.path(),
            &["agent", "export", "demo", "--out", pkg.to_str().unwrap()],
        ),
        "export",
    );

    // Fresh home with an EMPTY registry → --model ref cannot resolve.
    let dst_home = TempDir::new().unwrap();
    let dst_bin = TempDir::new().unwrap();
    let install = run(
        dst_home.path(),
        dst_bin.path(),
        &[
            "agent",
            "install",
            pkg.to_str().unwrap(),
            "--model",
            "does_not_exist",
        ],
    );
    assert!(
        !install.status.success(),
        "install with an unregistered --model ref should fail"
    );
    let err = String::from_utf8_lossy(&install.stderr);
    assert!(
        err.contains("not found"),
        "error should mention the missing ref, got: {err}"
    );
}
