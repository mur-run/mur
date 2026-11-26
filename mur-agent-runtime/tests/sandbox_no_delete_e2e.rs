//! #2582 kernel e2e: `filesystem.no_delete` under the real sandbox (SBPL on
//! macOS, Landlock on Linux). Split from `sandbox_e2e.rs` to keep both files
//! under the 800-line limit; the skip/fail contract is the same.

/// Turns "sandbox not enforcing" from a skip into a failure (CI sets it).
#[cfg(any(target_os = "macos", target_os = "linux"))]
const REQUIRE_SANDBOX_ENV: &str = "MUR_TEST_REQUIRE_SANDBOX";

/// Exit code for "sandbox required but did not seal", distinct from 0/1.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const EXIT_SANDBOX_REQUIRED: i32 = 4;

/// Skip (exit 0) when the seal did not apply; fail when
/// `MUR_TEST_REQUIRE_SANDBOX=1`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn skip_or_fail_unenforced(why: &dyn std::fmt::Debug) -> ! {
    if std::env::var_os(REQUIRE_SANDBOX_ENV).is_some_and(|v| v == "1") {
        eprintln!("FAIL: {REQUIRE_SANDBOX_ENV}=1 but sandbox not enforcing: {why:?}");
        std::process::exit(EXIT_SANDBOX_REQUIRED);
    }
    eprintln!("SKIP: sandbox not enforcing: {why:?}");
    std::process::exit(0);
}

/// #2582: a `no_delete` path stays writable while every spelling of a delete
/// fails under the real kernel — the shell `rm`, `os.remove` and
/// `shutil.rmtree` from an interpreter (the #2581 bypass), and a rename out.
/// The controls (create, edit, delete beside the protected dir) are
/// load-bearing: without them an EPERM could mean the grant never applied.
#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn no_delete_path_refuses_every_delete_spelling() {
    let root = tempfile::tempdir().unwrap();
    // SBPL matches resolved paths; macOS tempdirs sit behind /var -> /private/var.
    let root = root.keep().canonicalize().unwrap();
    let exe = std::env::current_exe().unwrap();
    let out = std::process::Command::new(&exe)
        .env("MUR_TEST_SANDBOX_NO_DELETE", &root)
        .env_remove("MUR_AGENT_SKIP_SANDBOX")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        out.status.code(),
        Some(0),
        "no_delete must block removal but not writing:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Subprocess entry point for `no_delete_path_refuses_every_delete_spelling`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[ctor::ctor]
fn sandbox_no_delete_subprocess_main() {
    let Some(root) = std::env::var_os("MUR_TEST_SANDBOX_NO_DELETE") else {
        return;
    };
    use mur_agent_runtime::sandbox;
    use mur_common::agent::{AgentProfile, SpawnMode};
    use std::path::PathBuf;
    use std::process::Command;

    let root = PathBuf::from(root);
    let agent_home = root.join("mur/agents/tester");
    let work = root.join("work");
    let keep = work.join("keep");
    for d in [&agent_home, &keep.join("tree/sub"), &work.join("scratch")] {
        std::fs::create_dir_all(d).unwrap();
    }
    for f in ["a.txt", "b.txt", "c.txt", "d.txt", "tree/sub/x"] {
        std::fs::write(keep.join(f), b"keep").unwrap();
    }
    std::fs::write(work.join("scratch/gone.txt"), b"x").unwrap();

    let mut profile = AgentProfile::default_for_tests();
    let fs = &mut profile.entitlements.filesystem;
    fs.write.push(work.to_string_lossy().into_owned());
    fs.no_delete.push(keep.to_string_lossy().into_owned());
    // Review P1 on #2595: an entry that does not exist yet, named through a
    // symlinked parent, must protect the real directory once it appears.
    let alias = root.join("alias");
    std::os::unix::fs::symlink(&work, &alias).unwrap();
    fs.no_delete
        .push(alias.join("future").to_string_lossy().into_owned());
    profile.entitlements.processes.spawn.mode = SpawnMode::Any;
    match sandbox::apply(&profile.entitlements, &agent_home, &[], &[], &[]) {
        Ok(s) if s.enforcing => {}
        other => skip_or_fail_unenforced(&other),
    }

    let mut failures = Vec::new();
    let mut expect = |what: &str, ok: bool, want_ok: bool| {
        if ok != want_ok {
            failures.push(format!(
                "{what}: {} (expected {})",
                if ok { "succeeded" } else { "failed" },
                if want_ok { "success" } else { "EPERM" }
            ));
        }
    };
    let run = |prog: &str, args: &[&str]| {
        Command::new(prog)
            .args(args)
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    let k = |f: &str| keep.join(f).to_string_lossy().into_owned();

    // Controls: writing inside, and deleting beside, must still work.
    expect(
        "create inside",
        std::fs::write(keep.join("new.txt"), b"n").is_ok(),
        true,
    );
    expect(
        "append inside",
        std::fs::OpenOptions::new()
            .append(true)
            .open(keep.join("a.txt"))
            .and_then(|mut f| std::io::Write::write_all(&mut f, b"+"))
            .is_ok(),
        true,
    );
    expect(
        "mkdir inside",
        std::fs::create_dir(keep.join("d2")).is_ok(),
        true,
    );
    expect(
        "delete beside",
        std::fs::remove_file(work.join("scratch/gone.txt")).is_ok(),
        true,
    );

    // Every spelling of a delete.
    expect(
        "rust remove_file",
        std::fs::remove_file(keep.join("a.txt")).is_ok(),
        false,
    );
    expect("rm", run("rm", &["-f", &k("b.txt")]), false);
    expect("rm -rf", run("rm", &["-rf", &k("tree")]), false);
    expect(
        "python os.remove",
        run(
            "python3",
            &["-c", &format!("import os; os.remove({:?})", k("c.txt"))],
        ),
        false,
    );
    expect(
        "python shutil.rmtree",
        run(
            "python3",
            &[
                "-c",
                &format!("import shutil; shutil.rmtree({:?})", k("tree")),
            ],
        ),
        false,
    );
    expect(
        "rename out",
        std::fs::rename(keep.join("d.txt"), work.join("scratch/d.txt")).is_ok(),
        false,
    );
    expect(
        "rmdir empty",
        std::fs::remove_dir(keep.join("d2")).is_ok(),
        false,
    );
    // Created after the seal, reached through the real path.
    let future = work.join("future");
    expect(
        "mkdir future (after seal)",
        std::fs::create_dir_all(future.join("sub")).is_ok(),
        true,
    );
    expect(
        "write in future",
        std::fs::write(future.join("sub/f.txt"), b"f").is_ok(),
        true,
    );
    expect(
        "delete in future via real path",
        std::fs::remove_file(future.join("sub/f.txt")).is_ok(),
        false,
    );
    expect(
        "rm -rf future via real path",
        run("rm", &["-rf", &future.to_string_lossy()]),
        false,
    );
    expect(
        "future file still present",
        future.join("sub/f.txt").exists(),
        true,
    );
    // Nothing above may have taken a file with it.
    for f in ["a.txt", "b.txt", "c.txt", "d.txt", "tree/sub/x"] {
        expect(&format!("{f} still present"), keep.join(f).exists(), true);
    }

    if failures.is_empty() {
        std::process::exit(0);
    }
    for f in &failures {
        eprintln!("ERROR: {f}");
    }
    std::process::exit(1);
}
