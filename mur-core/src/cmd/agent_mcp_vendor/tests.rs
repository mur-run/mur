use super::*;

/// Vendoring rewrote the launch command and moved the script; a profile
/// that does not carry both grants produces an entry that cannot start.
#[test]
fn vendoring_grants_the_interpreter_and_the_install_dir() {
    let mut p = AgentProfile::default_for_tests();
    let dir = Path::new("/home/u/.mur/mcp-packages/a1/dc");
    sync_launch_entitlements(&mut p, "node", dir);
    assert!(
        p.entitlements
            .processes
            .spawn
            .allowed
            .iter()
            .any(|a| a == "node")
    );
    assert!(
        p.entitlements
            .filesystem
            .read
            .iter()
            .any(|r| r == "/home/u/.mur/mcp-packages/a1/dc"),
    );
}

/// Re-vendoring the same server must not grow the lists.
#[test]
fn syncing_twice_is_idempotent() {
    let mut p = AgentProfile::default_for_tests();
    let dir = Path::new("/home/u/.mur/mcp-packages/a1/dc");
    sync_launch_entitlements(&mut p, "node", dir);
    let spawn = p.entitlements.processes.spawn.allowed.len();
    let read = p.entitlements.filesystem.read.len();
    sync_launch_entitlements(&mut p, "node", dir);
    assert_eq!(p.entitlements.processes.spawn.allowed.len(), spawn);
    assert_eq!(p.entitlements.filesystem.read.len(), read);
}

/// An existing ANCESTOR grant is not a substitute: a read grant that
/// reaches the credential store is dropped whole by the sandbox, so the
/// narrow path must be added even when a wider one is already listed.
#[test]
fn an_ancestor_grant_does_not_stand_in_for_the_install_dir() {
    let mut p = AgentProfile::default_for_tests();
    p.entitlements.filesystem.read.push("/home/u/.mur".into());
    sync_launch_entitlements(&mut p, "node", Path::new("/home/u/.mur/mcp-packages/a1/dc"));
    assert!(
        p.entitlements
            .filesystem
            .read
            .iter()
            .any(|r| r == "/home/u/.mur/mcp-packages/a1/dc"),
    );
}

fn write_pkg(dir: &Path, name: &str, bin: serde_json::Value) {
    let p = dir.join("node_modules").join(name);
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(
        p.join("package.json"),
        serde_json::json!({ "name": name, "bin": bin }).to_string(),
    )
    .unwrap();
}

fn touch(dir: &Path, name: &str, rel: &str) {
    let f = dir.join("node_modules").join(name).join(rel);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, b"// entry\n").unwrap();
}

#[test]
fn resolves_a_string_bin() {
    let d = tempfile::tempdir().unwrap();
    write_pkg(d.path(), "solo", serde_json::json!("dist/index.js"));
    touch(d.path(), "solo", "dist/index.js");
    let got = resolve_bin(d.path(), "solo").unwrap();
    assert!(got.ends_with("dist/index.js"));
    assert!(got.is_absolute(), "the launch path must not depend on cwd");
}

#[test]
fn picks_the_bin_matching_the_scoped_package_name() {
    let d = tempfile::tempdir().unwrap();
    write_pkg(
        d.path(),
        "@yawlabs/fetch-mcp",
        serde_json::json!({ "fetch-mcp": "dist/index.js", "other": "dist/other.js" }),
    );
    touch(d.path(), "@yawlabs/fetch-mcp", "dist/index.js");
    let got = resolve_bin(d.path(), "@yawlabs/fetch-mcp").unwrap();
    assert!(got.ends_with("dist/index.js"));
}

/// Guessing which of several binaries to launch would silently run the
/// wrong program, so an ambiguous `bin` map has to fail loudly.
#[test]
fn refuses_an_ambiguous_bin_map() {
    let d = tempfile::tempdir().unwrap();
    write_pkg(
        d.path(),
        "many",
        serde_json::json!({ "a": "a.js", "b": "b.js" }),
    );
    let err = resolve_bin(d.path(), "many").unwrap_err().to_string();
    assert!(err.contains("exactly one"), "got: {err}");
}

#[test]
fn reports_a_bin_path_that_does_not_exist() {
    let d = tempfile::tempdir().unwrap();
    write_pkg(d.path(), "ghost", serde_json::json!("dist/missing.js"));
    let err = resolve_bin(d.path(), "ghost").unwrap_err().to_string();
    assert!(err.contains("does not exist"), "got: {err}");
}

#[test]
fn a_package_with_no_bin_cannot_be_vendored() {
    let d = tempfile::tempdir().unwrap();
    write_pkg(d.path(), "lib-only", serde_json::Value::Null);
    let err = resolve_bin(d.path(), "lib-only").unwrap_err().to_string();
    assert!(err.contains("no `bin`"), "got: {err}");
}

#[test]
fn lockfile_hash_changes_with_the_lockfile() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("package-lock.json"), b"{\"v\":1}").unwrap();
    let a = lockfile_sha256(d.path()).unwrap();
    std::fs::write(d.path().join("package-lock.json"), b"{\"v\":2}").unwrap();
    let b = lockfile_sha256(d.path()).unwrap();
    assert_ne!(a, b, "a changed dependency tree must change the pin");
    assert_eq!(a.len(), 64);
}

#[test]
fn install_dir_is_scoped_per_agent_and_server() {
    let home = Path::new("/tmp/murhome");
    assert_ne!(
        install_dir(home, "a1", "fetch"),
        install_dir(home, "a2", "fetch"),
        "two agents must not share one install they can both invalidate",
    );
}

// ── Registry signature audit ────────────────────────────────────────────

/// The shape npm 10 actually returns for a clean tree, captured from a real
/// run against @yawlabs/fetch-mcp (105 packages, all verified).
#[test]
fn a_clean_audit_reports_nothing_missing_and_nothing_invalid() {
    let a = parse_audit(r#"{"invalid":[],"missing":[]}"#).expect("clean body parses");
    assert!(a.invalid.is_empty());
    assert_eq!(a.missing, 0);
}

/// Unsigned packages are common for older releases — worth recording, not
/// worth refusing over.
#[test]
fn unsigned_packages_are_counted_not_fatal() {
    let a =
        parse_audit(r#"{"invalid":[],"missing":[{"name":"old-pkg"},{"name":"older"}]}"#).unwrap();
    assert_eq!(a.missing, 2);
    assert!(a.invalid.is_empty(), "missing is not invalid");
}

/// An invalid signature means the bytes on disk are not what the registry
/// signed. `vendor_entry` refuses on this, so the names have to survive
/// parsing to reach the user.
#[test]
fn invalid_signatures_keep_their_package_names() {
    let a = parse_audit(r#"{"invalid":[{"name":"evil-dep"}],"missing":[]}"#).unwrap();
    assert_eq!(a.invalid, vec!["evil-dep".to_string()]);
}

#[test]
fn an_unreadable_audit_body_is_not_silently_treated_as_clean() {
    assert!(parse_audit("npm ERR! code ENOTFOUND").is_none());
    assert!(parse_audit("").is_none());
}

/// A future npm that renames or drops the field must not turn into a
/// confident "all verified".
#[test]
fn missing_keys_degrade_to_empty_rather_than_inventing_results() {
    let a = parse_audit(r#"{}"#).expect("valid json still parses");
    assert!(a.invalid.is_empty());
    assert_eq!(a.missing, 0);
    let a = parse_audit(r#"{"invalid":[{}],"missing":[]}"#).unwrap();
    assert_eq!(
        a.invalid,
        vec!["<unnamed>".to_string()],
        "an entry without a name still has to be reported, not dropped",
    );
}

// ── Provenance ──────────────────────────────────────────────────────────

/// The shape npm returns for a release published with provenance, captured
/// from real `npm view sigstore dist.attestations --json` output.
#[test]
fn provenance_predicate_type_is_extracted() {
    let body = r#"{
          "url": "https://registry.npmjs.org/-/npm/v1/attestations/sigstore@5.0.0",
          "provenance": { "predicateType": "https://slsa.dev/provenance/v1" }
        }"#;
    assert_eq!(
        parse_provenance(body).as_deref(),
        Some("https://slsa.dev/provenance/v1"),
    );
}

/// Most releases publish none — npm prints nothing at all. That is the
/// common case, not a failure, and must not read as an error.
#[test]
fn no_attestations_is_absence_not_failure() {
    assert_eq!(parse_provenance(""), None);
    assert_eq!(parse_provenance("{}"), None);
    assert_eq!(
        parse_provenance(r#"{"url":"x"}"#),
        None,
        "url without provenance"
    );
    assert_eq!(
        parse_provenance(r#"{"provenance":{}}"#),
        None,
        "no predicateType"
    );
    assert_eq!(parse_provenance("npm ERR! 404"), None);
}

// ── PyPI console scripts ────────────────────────────────────────────────

/// Real `entry_points.txt` from mcp-server-time, installed via uv.
#[test]
fn reads_the_console_script_from_entry_points() {
    let body = "[console_scripts]\nmcp-server-time = mcp_server_time:main\n";
    assert_eq!(console_scripts(body), vec!["mcp-server-time".to_string()]);
}

/// Other sections declare entry points that are not launchable servers;
/// treating a `gui_scripts` entry or a pytest plugin as the server would
/// launch the wrong program.
#[test]
fn only_console_scripts_count() {
    let body = "[console_scripts]\nreal-server = pkg:main\n\n[gui_scripts]\nnot-a-server = pkg:gui\n\n[pytest11]\nplugin = pkg.plugin\n";
    assert_eq!(console_scripts(body), vec!["real-server".to_string()]);
}

#[test]
fn several_console_scripts_are_ambiguous_and_the_caller_must_refuse() {
    let body = "[console_scripts]\na = pkg:a\nb = pkg:b\n";
    assert_eq!(console_scripts(body).len(), 2);
}

#[test]
fn no_console_scripts_section_yields_nothing() {
    assert!(console_scripts("[gui_scripts]\nx = pkg:x\n").is_empty());
    assert!(console_scripts("").is_empty());
}

/// The lockfile name must follow the runner. A Python entry checked against
/// `package-lock.json` would find no file — and a missing file reads as
/// "install gone", not as drift, so the mistake would pass quietly.
#[test]
fn lockfile_name_follows_the_runner() {
    let pin = |runner: &str| McpPackagePin {
        runner: runner.into(),
        name: "x".into(),
        version: "1".into(),
        install_dir: "/tmp/i".into(),
        lockfile_sha256: "h".into(),
        ..Default::default()
    };
    assert_eq!(pin("npm").lockfile_name(), "package-lock.json");
    assert_eq!(pin("pypi").lockfile_name(), "requirements.lock");
    assert!(pin("pypi").lockfile_path().ends_with("requirements.lock"));
}
