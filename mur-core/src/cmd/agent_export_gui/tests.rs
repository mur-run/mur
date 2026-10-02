use super::*;

#[test]
fn sanitize_strips_special_chars() {
    assert_eq!(sanitize_for_bundle_id("My Agent!"), "my-agent");
    assert_eq!(sanitize_for_bundle_id("__research__"), "research");
    assert_eq!(sanitize_for_bundle_id("agent-1"), "agent-1");
    assert_eq!(sanitize_for_bundle_id("Hello.World"), "hello-world");
}

#[test]
fn host_target_known_for_dev_machine() {
    let triple = host_target_triple().expect("host detected");
    assert!(
        triple.contains("apple-darwin") || triple.contains("linux") || triple.contains("windows"),
        "unexpected triple: {triple}"
    );
}

#[test]
fn embedded_metadata_round_trips() {
    let meta = EmbeddedMetadata {
        schema_version: 1,
        agent_name: "demo".into(),
        display_name: "Demo".into(),
        mode: BundleMode::Template,
        theme_default: "dark".into(),
        mur_version: "2.4.1".into(),
    };
    let json = serde_json::to_string(&meta).unwrap();
    let back: EmbeddedMetadata = serde_json::from_str(&json).unwrap();
    assert_eq!(back.agent_name, "demo");
    assert_eq!(back.mode, BundleMode::Template);
}

// ─── WCAG AA validator (build-time gate inside phase_3) ──────

#[test]
fn wcag_passes_for_high_contrast_palette() {
    let theme = serde_json::json!({
        "name": "test-pass",
        "colors": {
            "bg": "#000000",
            "fg": "#ffffff",
            "fg_secondary": "#cccccc",
            "accent": "#ffff00",
            "accent_fg": "#000000",
            "border": "#888888"
        }
    });
    let failures = wcag_contrast_failures(&theme).expect("colors object present");
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");
}

#[test]
fn wcag_flags_low_contrast_body_text() {
    // fg #444 vs bg #555 → 1.04:1 ratio (way below 4.5:1).
    let theme = serde_json::json!({
        "name": "test-fail",
        "colors": {
            "bg": "#555555",
            "fg": "#444444",
            "accent": "#000000",
            "accent_fg": "#ffffff",
            "border": "#222222"
        }
    });
    let failures = wcag_contrast_failures(&theme).expect("colors object present");
    assert!(
        failures.iter().any(|f| f.contains("body text")),
        "expected a body-text failure, got: {failures:?}"
    );
}

#[test]
fn wcag_returns_none_when_colors_block_missing() {
    let theme = serde_json::json!({"name": "no-colors"});
    assert!(wcag_contrast_failures(&theme).is_none());
}

#[test]
fn wcag_skips_pairs_where_one_color_is_absent() {
    // accent_fg is missing — that pair should be skipped, not error.
    let theme = serde_json::json!({
        "name": "partial",
        "colors": {
            "bg": "#000000",
            "fg": "#ffffff",
            "accent": "#ffff00",
            "border": "#888888"
        }
    });
    let failures = wcag_contrast_failures(&theme).expect("colors present");
    // No failures expected — fg/bg, border/bg are both fine; accent_fg
    // pair is skipped.
    assert!(failures.is_empty(), "expected pass, got: {failures:?}");
}

#[test]
fn strip_identity_removes_sensitive_files_and_keeps_others() {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let tmp = std::env::temp_dir().join(format!("mur-strip-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    // Build a fake source tarball with all the sensitive
    // names + one innocent file.
    let src = tmp.join("source.tar.gz");
    {
        let f = std::fs::File::create(&src).unwrap();
        let enc = GzEncoder::new(f, Compression::default());
        let mut t = tar::Builder::new(enc);
        for (path, body) in [
            ("identity.key", b"SECRET" as &[u8]),
            ("identity.pub", b"z123pub"),
            ("rotations.jsonl", b"{\"rotation\":1}"),
            ("profile.yaml", b"name: demo"),
            ("sys_prompt.md", b"You are a demo agent."),
            ("skills/web.md", b"# Web skill"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            t.append_data(&mut header, path, body).unwrap();
        }
        let enc = t.into_inner().unwrap();
        enc.finish().unwrap();
    }

    let dst = tmp.join("stripped.tar.gz");
    super::strip_identity_from_tarball(&src, &dst).unwrap();

    // Re-read the stripped tarball.
    let f = std::fs::File::open(&dst).unwrap();
    let mut a = tar::Archive::new(flate2::read::GzDecoder::new(f));
    let mut paths = Vec::new();
    for entry in a.entries().unwrap() {
        let entry = entry.unwrap();
        paths.push(entry.path().unwrap().to_string_lossy().to_string());
    }
    paths.sort();

    assert!(
        !paths.iter().any(|p| p.contains("identity.key")),
        "identity.key should be stripped: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.contains("identity.pub")),
        "identity.pub should be stripped: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.contains("rotations.jsonl")),
        "rotations.jsonl should be stripped: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p == "profile.yaml"),
        "profile.yaml must survive: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p == "sys_prompt.md"),
        "sys_prompt.md must survive: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p == "skills/web.md"),
        "skills/ tree must survive: {paths:?}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn clone_mode_refuses_without_unsafe_env() {
    // Build options with clone_identity=true. We can't easily
    // exercise the full pipeline (needs a real agent home + tauri
    // toolchain); but the gate runs as the very first thing in
    // run() before any I/O, so we test it by calling the run()
    // entry directly with a path that doesn't exist — the gate
    // should fire before reaching the prereq_check or the agent
    // home read.
    // SAFETY: tests run serially in a single thread per default.
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.unset_var("MUR_ALLOW_UNSAFE_CLONE");
    let opts = ExportGuiOptions {
        agent_name: "demo".into(),
        agent_home: PathBuf::from("/nonexistent"),
        out: PathBuf::from("/nonexistent/out"),
        theme: "light".into(),
        icon: None,
        clone_identity: true,
        skip_notarize: true,
    };
    let err = run(opts).unwrap_err().to_string();
    assert!(
        err.contains("clone-mode bundle would ship") || err.contains("MUR_ALLOW_UNSAFE_CLONE"),
        "expected clone-mode safety gate, got: {err}"
    );
}

#[test]
fn wcag_treats_invalid_hex_as_skip() {
    let theme = serde_json::json!({
        "name": "bad-hex",
        "colors": {
            "bg": "#000000",
            "fg": "not-a-color",
            "accent": "#ffff00",
            "accent_fg": "#000000",
            "border": "#888888"
        }
    });
    // fg is unparseable → fg/bg pair is skipped silently. Should
    // not panic, should not produce a failure for that pair.
    let failures = wcag_contrast_failures(&theme).expect("colors present");
    assert!(
        !failures.iter().any(|f| f.contains("body text")),
        "expected fg/bg pair to be skipped on bad hex, got: {failures:?}"
    );
}
