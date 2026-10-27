//! `write_protected_paths()` under a real seal (startup-permission-overrides
//! spec, §Testing: R5-1, R6-1, R7-1, R7-7, N1).
//!
//! Each test re-execs this binary; a `#[ctor]` hook in the child seals itself
//! as agent `<home>/agents/x` and probes through `sh`, so the kernel judges a
//! spawned process, not the file tools.
//!
//! The spec phrases R6-1 as "a declared write grant on `<mur_home>`". That
//! profile never seals: E2 (`sandbox::protected`) refuses any grant that
//! reaches `channel-heads/` (`mur_home_grant_is_refused` pins this). So every
//! protected target gets its OWN direct write grant instead, which is the
//! stricter test: the grant covers exactly the path that must stay closed.
//!
//! The fake MUR home lives under `CARGO_TARGET_TMPDIR`, canonicalized (R7-4):
//! macOS write-exempts `/private/var/folders` and `/private/tmp`, and Seatbelt
//! matches `subpath` against the resolved path.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const CASE_ENV: &str = "MUR_TEST_WRITE_PROTECTED_CASE";
const ROOT_ENV: &str = "MUR_TEST_WRITE_PROTECTED_ROOT";
const REQUIRE_SANDBOX_ENV: &str = "MUR_TEST_REQUIRE_SANDBOX";
const EXIT_SANDBOX_REQUIRED: i32 = 4;
const AGENT: &str = "x";
const PEER: &str = "y";
const FLEET: &str = "f";
const PROBE: &str = "probe";
const SEED: &[u8] = b"seed";
const CAPTURE_STORES: [&str; 5] = ["queue", "session", "conversations", "telemetry", "traces"];
const CONVERSATIONS: &str = "conversations";
const PRIVATE: &str = "private";

/// `<root>/mur` is MUR_HOME, `<root>/home` is HOME.
struct Layout {
    mur: PathBuf,
    home: PathBuf,
}

impl Layout {
    fn at(root: &Path) -> Self {
        Self {
            mur: root.join("mur"),
            home: root.join("home"),
        }
    }

    fn agent_home(&self) -> PathBuf {
        self.mur.join("agents").join(AGENT)
    }

    fn commander(&self) -> PathBuf {
        self.mur.join("commander")
    }
}

fn fresh_root(case: &str) -> PathBuf {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("write_protected_seal");
    std::fs::create_dir_all(&base).unwrap();
    let root = std::fs::canonicalize(&base)
        .unwrap()
        .join(format!("{case}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let l = Layout::at(&root);
    for n in [AGENT, PEER] {
        std::fs::create_dir_all(l.mur.join("agents").join(n)).unwrap();
    }
    std::fs::create_dir_all(&l.home).unwrap();
    root
}

fn run_case(case: &str) {
    let root = fresh_root(case);
    let l = Layout::at(&root);
    let out = Command::new(std::env::current_exe().unwrap())
        .env(CASE_ENV, case)
        .env(ROOT_ENV, &root)
        .env("HOME", &l.home)
        .env("MUR_HOME", &l.mur)
        .env_remove("MUR_AGENT_SKIP_SANDBOX")
        .env_remove("MUR_AGENT_BIN_DIR")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    eprintln!("{stderr}");
    assert_eq!(out.status.code(), Some(0), "case {case} failed:\n{stderr}");
}

/// Why the tests below grant each target directly: a `<mur_home>` grant is
/// refused before anything is sealed.
#[test]
fn mur_home_grant_is_refused() {
    run_case("mur_home_refused");
}

/// R5-1: every extra write row stays closed under a write grant aimed right
/// at it, while the agent's own home and an ordinary grant stay writable.
#[test]
fn extra_write_rows_stay_closed_under_a_direct_grant() {
    run_case("extra_rows");
}

/// R6-1, R7-1: without fleet_run no capture store is writable.
#[test]
fn capture_stores_stay_closed_without_fleet_run() {
    run_case("capture_nonfleet");
}

/// R6-1, R7-1, R7-7: with fleet_run only `conversations/` opens, and a user
/// `fs_deny` inside it still wins (macOS).
#[test]
fn fleet_run_opens_only_conversations() {
    run_case("capture_fleet");
}

/// N1: a fleet-enabled agent reads the pinned commander public keys (so the
/// loop is governed) and nothing else in `commander/`, and writes none of it.
#[test]
fn fleet_child_reads_commander_public_keys_only() {
    run_case("commander_fleet");
}

/// A write grant named by a symlink into `secrets/` must not open it: the
/// kernel resolves the link, so the partition has to as well. An unrelated
/// grant beside it stays writable.
#[test]
fn a_symlinked_grant_cannot_reach_secrets() {
    run_case("symlink_grant");
}

// ── child side ─────────────────────────────────────────────────────────────

fn skip_or_fail_unenforced(why: &dyn std::fmt::Debug) -> ! {
    if std::env::var_os(REQUIRE_SANDBOX_ENV).is_some_and(|v| v == "1") {
        eprintln!("FAIL: {REQUIRE_SANDBOX_ENV}=1 but sandbox not enforcing: {why:?}");
        std::process::exit(EXIT_SANDBOX_REQUIRED);
    }
    eprintln!("SKIP: sandbox not enforcing: {why:?}");
    std::process::exit(0);
}

fn fail(msg: &str) -> ! {
    eprintln!("ERROR: {msg}");
    std::process::exit(1);
}

/// Output is piped, never sent to `/dev/null`: Landlock grants `/dev/null`
/// read only, so a redirect there would fail every probe for the wrong reason.
fn sh(script: &str, path: &Path) -> bool {
    let out = Command::new("sh")
        .arg("-c")
        .arg(script)
        .arg("sh")
        .arg(path)
        .output()
        .unwrap_or_else(|e| fail(&format!("spawn sh: {e}")));
    out.status.success()
}

/// Write `target` exactly, through a spawned shell. Callers name the file:
/// deciding "is this a directory" inside the seal would need a `stat`, which
/// macOS denies on read-protected paths, and the probe would then aim at the
/// wrong path and fail for the wrong reason.
fn can_write(target: &Path) -> bool {
    sh(r#"printf x > "$1""#, target)
}

/// A new file inside `dir`.
fn inside(dir: &Path) -> PathBuf {
    dir.join(PROBE)
}

fn can_read_file(p: &Path) -> bool {
    sh(r#"cat "$1""#, p)
}

fn can_list(p: &Path) -> bool {
    sh(r#"ls "$1""#, p)
}

/// Collects every mismatch so one run reports all of them.
#[derive(Default)]
struct Expect(Vec<String>);

impl Expect {
    fn write(&mut self, p: &Path, want: bool) {
        let got = can_write(p);
        if got != want {
            self.0
                .push(format!("write {} = {got}, want {want}", p.display()));
        }
    }

    fn read(&mut self, p: &Path, want: bool) {
        let got = can_read_file(p);
        if got != want {
            self.0
                .push(format!("read {} = {got}, want {want}", p.display()));
        }
    }

    fn list(&mut self, p: &Path, want: bool) {
        let got = can_list(p);
        if got != want {
            self.0
                .push(format!("list {} = {got}, want {want}", p.display()));
        }
    }

    fn check(&mut self, ok: bool, what: String) {
        if !ok {
            self.0.push(what);
        }
    }

    fn finish(self) -> ! {
        if self.0.is_empty() {
            std::process::exit(0);
        }
        fail(&self.0.join("\n  "));
    }
}

fn seed_file(p: &Path) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, SEED).unwrap();
}

fn enable_fleet_run(l: &Layout) {
    std::fs::write(
        l.mur.join("config.yaml"),
        format!("fleet_run:\n  agents: [{AGENT}]\n  fleets: [{FLEET}]\n"),
    )
    .unwrap();
}

fn seal(
    write: &[PathBuf],
    deny: &[PathBuf],
    l: &Layout,
) -> mur_agent_runtime::sandbox::SandboxStatus {
    let mut profile = mur_common::agent::AgentProfile::default_for_tests();
    let fs = &mut profile.entitlements.filesystem;
    fs.write = write.iter().map(|p| p.display().to_string()).collect();
    fs.deny.extend(deny.iter().map(|p| p.display().to_string()));
    match mur_agent_runtime::sandbox::apply(&profile.entitlements, &l.agent_home(), &[], &[], &[]) {
        Ok(s) if s.enforcing => s,
        other => skip_or_fail_unenforced(&other),
    }
}

#[ctor::ctor]
fn write_protected_seal_subprocess_main() {
    let Some(case) = std::env::var_os(CASE_ENV) else {
        return;
    };
    let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("root env"));
    let l = Layout::at(&root);
    match case.to_str().unwrap_or_default() {
        "mur_home_refused" => mur_home_refused(&l),
        "extra_rows" => extra_rows(&l),
        "capture_nonfleet" => capture_nonfleet(&l),
        "capture_fleet" => capture_fleet(&l),
        "commander_fleet" => commander_fleet(&l),
        "symlink_grant" => symlink_grant(&l),
        other => fail(&format!("unknown case {other}")),
    }
}

fn mur_home_refused(l: &Layout) -> ! {
    let mut profile = mur_common::agent::AgentProfile::default_for_tests();
    profile.entitlements.filesystem.write = vec![l.mur.display().to_string()];
    match mur_agent_runtime::sandbox::apply(&profile.entitlements, &l.agent_home(), &[], &[], &[]) {
        Err(e) if mur_agent_runtime::sandbox::protected::is_refusal(&e) => std::process::exit(0),
        other => fail(&format!(
            "a <mur_home> write grant must be refused, got {other:?}"
        )),
    }
}

fn extra_rows(l: &Layout) -> ! {
    let c = l.commander();
    // File rows, pre-seeded so a write is an overwrite of something real.
    let files = [
        l.mur.join("auth.json"),
        l.mur.join("identity.key"),
        c.join("identity.key"),
        c.join("signing.key"),
        c.join(".env"),
        c.join("identity.pub"),
        c.join("identity.prev.pub"),
        l.mur.join("publisher").join("identity.key"),
        l.mur.join("mobile").join("pair-token"),
        l.mur.join("runtime").join("vlc.json"),
    ];
    for f in &files {
        seed_file(f);
    }
    // Subtree rows.
    let dirs = [
        l.mur.join("keys"),
        l.mur.join("secrets"),
        l.mur.join("actions-runner"),
        l.home.join(".ssh"),
        l.home.join(".aws"),
        l.home.join(".gnupg"),
    ];
    for d in &dirs {
        std::fs::create_dir_all(d).unwrap();
    }
    // Parents granted so the FILE rows are carved out of a live grant.
    let parents = [
        c.clone(),
        l.mur.join("publisher"),
        l.mur.join("mobile"),
        l.mur.join("runtime"),
    ];
    let control = l.mur.join("work");
    std::fs::create_dir_all(&control).unwrap();

    let mut grants: Vec<PathBuf> = vec![l.mur.join("auth.json"), l.mur.join("identity.key")];
    grants.extend(parents.iter().cloned());
    grants.extend(dirs.iter().cloned());
    grants.push(control.clone());
    let status = seal(&grants, &[], l);

    let mut e = Expect::default();
    e.write(&inside(&l.agent_home()), true);
    e.write(&inside(&control), true);
    e.write(&inside(&l.mur.join("agents").join(PEER)), false);
    for f in &files {
        e.write(f, false);
    }
    for d in &dirs {
        e.write(&inside(d), false);
    }
    // A sibling of a protected file, inside a granted parent: macOS carves the
    // file out of the grant, Landlock cannot carve and drops the grant whole.
    let carved = cfg!(target_os = "macos");
    for p in &parents {
        e.write(&p.join("sibling"), carved);
    }
    for f in &files {
        let unchanged = std::fs::read(f).is_ok_and(|b| b == SEED);
        // macOS also read-denies these, so `read` failing is not a change.
        let readable = std::fs::read(f).is_ok();
        e.check(
            !readable || unchanged,
            format!("{} was modified", f.display()),
        );
    }
    if cfg!(target_os = "linux") {
        let dropped: Vec<&str> = status
            .dropped
            .iter()
            .filter(|d| d.verb == "write")
            .map(|d| d.path.as_str())
            .collect();
        for g in grants.iter().filter(|g| **g != control) {
            e.check(
                dropped.contains(&g.display().to_string().as_str()),
                format!(
                    "write grant {} not listed in dropped: {dropped:?}",
                    g.display()
                ),
            );
        }
    }
    e.finish();
}

fn capture_nonfleet(l: &Layout) -> ! {
    let stores: Vec<PathBuf> = CAPTURE_STORES.iter().map(|s| l.mur.join(s)).collect();
    for s in &stores {
        std::fs::create_dir_all(s).unwrap();
    }
    // macOS keeps a write-deny on every store even under a direct grant. On
    // Linux a direct grant on a capture store is the spec's documented residue
    // (it is not on the write list), so there the check is that nothing the
    // runtime adds on its own opens one.
    let grants: Vec<PathBuf> = if cfg!(target_os = "macos") {
        stores.clone()
    } else {
        Vec::new()
    };
    seal(&grants, &[], l);
    let mut e = Expect::default();
    e.write(&inside(&l.agent_home()), true);
    for s in &stores {
        e.write(&inside(s), false);
    }
    e.finish();
}

fn capture_fleet(l: &Layout) -> ! {
    enable_fleet_run(l);
    let conv = l.mur.join(CONVERSATIONS);
    let private = conv.join(PRIVATE);
    let others: Vec<PathBuf> = CAPTURE_STORES
        .iter()
        .filter(|s| **s != CONVERSATIONS)
        .map(|s| l.mur.join(s))
        .collect();
    std::fs::create_dir_all(&private).unwrap();
    for s in &others {
        std::fs::create_dir_all(s).unwrap();
    }
    let macos = cfg!(target_os = "macos");
    let grants: Vec<PathBuf> = if macos { others.clone() } else { Vec::new() };
    let deny: Vec<PathBuf> = if macos {
        vec![private.clone()]
    } else {
        Vec::new()
    };
    seal(&grants, &deny, l);

    let mut e = Expect::default();
    let written = inside(&conv);
    e.write(&inside(&l.agent_home()), true);
    e.write(&written, true);
    for s in &others {
        e.write(&inside(s), false);
    }
    if macos {
        // Read stays credential-denied (R6-1); the user's deny still wins (R7-1).
        e.list(&conv, false);
        e.read(&written, false);
        e.write(&inside(&private), false);
    } else {
        // Landlock installs the fleet grant with `AccessFs::from_all` (R7-7).
        e.list(&conv, true);
        e.read(&written, true);
        e.list(&l.mur.join("queue"), false);
    }
    e.finish();
}

fn commander_fleet(l: &Layout) -> ! {
    enable_fleet_run(l);
    let c = l.commander();
    let public = [c.join("identity.pub"), c.join("identity.prev.pub")];
    let secret = [c.join("identity.key"), c.join(".env")];
    for f in public.iter().chain(&secret) {
        seed_file(f);
    }
    seal(&[], &[], l);
    let mut e = Expect::default();
    for p in &public {
        e.read(p, true);
        e.write(p, false);
    }
    for s in &secret {
        e.read(s, false);
        e.write(s, false);
    }
    e.finish();
}

fn symlink_grant(l: &Layout) -> ! {
    let secrets = l.mur.join("secrets");
    let overrides = secrets.join("startup-overrides");
    std::fs::create_dir_all(&overrides).unwrap();
    let work = l.home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let link = work.join("granted-cache");
    std::os::unix::fs::symlink(&secrets, &link).unwrap();
    let plain = work.join("plain");
    std::fs::create_dir_all(&plain).unwrap();

    seal(&[link.clone(), plain.clone()], &[], l);
    let mut e = Expect::default();
    e.write(&inside(&link.join("startup-overrides")), false);
    e.write(&inside(&link), false);
    e.write(&inside(&overrides), false);
    e.write(&inside(&plain), true);
    e.finish()
}
