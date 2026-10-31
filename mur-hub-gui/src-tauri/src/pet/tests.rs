/// Pet windows live outside the `dashboard`/`popover` capabilities; without
/// their own capability the ACL silently denies `listen()`/`startDragging()`
/// and the pet is inert. Guard the capability file so it can't regress.
#[test]
fn pet_capability_covers_pet_windows() {
    let text = include_str!("../../capabilities/pet.json");
    let cap: serde_json::Value = serde_json::from_str(text).unwrap();

    let windows: Vec<&str> = cap["windows"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w.as_str())
        .collect();
    assert!(
        windows.contains(&"pet-*"),
        "capability must match pet-* windows"
    );

    let perms: Vec<&str> = cap["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p.as_str())
        .collect();
    // Every plugin command PetApp.tsx calls; app-defined commands need no ACL.
    for perm in [
        "core:event:allow-listen",
        "core:event:allow-unlisten",
        "core:window:allow-outer-position",
        "core:window:allow-start-dragging",
        // hide/show back the Hide-1h menu item (ACL-gated, was a silent no-op).
        "core:window:allow-hide",
        "core:window:allow-show",
    ] {
        assert!(perms.contains(&perm), "pet windows need {perm}");
    }
}

#[test]
fn cloud_provider_discloses_even_behind_loopback_proxy() {
    // The fix: anthropic routed via loopback cc-proxy must STILL disclose
    // (the proxy forwards to the cloud — the data leaves the machine).
    assert_eq!(
        super::classify_remote_provider("anthropic", Some("http://127.0.0.1:8088")),
        Some("Anthropic".into())
    );
    assert_eq!(
        super::classify_remote_provider("openai", Some("http://localhost:8088/v1")),
        Some("OpenAI".into())
    );
}

#[test]
fn cloud_provider_discloses_with_no_base_url() {
    assert_eq!(
        super::classify_remote_provider("anthropic", None),
        Some("Anthropic".into())
    );
}

#[test]
fn local_runtimes_never_disclose() {
    for p in ["ollama", "mlx", "lmstudio"] {
        assert_eq!(super::classify_remote_provider(p, None), None, "{p}");
        assert_eq!(
            super::classify_remote_provider(p, Some("http://127.0.0.1:11434")),
            None,
            "{p} loopback"
        );
    }
}

#[test]
fn unknown_provider_loopback_local_else_discloses() {
    // unknown + loopback = genuinely local (no proxy-to-cloud signal).
    assert_eq!(
        super::classify_remote_provider("homecorp", Some("http://127.0.0.1:9000")),
        None
    );
    assert_eq!(
        super::classify_remote_provider("homecorp", Some("http://[::1]:9000")),
        None
    );
    // unknown + non-loopback (or no url) = fail-toward-disclosure.
    assert_eq!(
        super::classify_remote_provider("homecorp", Some("https://api.homecorp.ai")),
        Some("the agent's model".into())
    );
    assert_eq!(
        super::classify_remote_provider("homecorp", None),
        Some("the agent's model".into())
    );
}
