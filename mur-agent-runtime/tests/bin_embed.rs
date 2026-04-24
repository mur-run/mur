use mur_agent_runtime::export::bin_embed;

#[test]
fn default_build_has_no_embedded_agent() {
    // Without the `embedded-agent` feature, the runtime must report no
    // embedded agent so the supervisor falls back to MUR_HOME discovery.
    assert!(
        !bin_embed::has_embedded_agent(),
        "default build must not claim to carry an embedded agent"
    );
}

#[test]
fn embedded_path_env_is_wired_for_build_rs() {
    // build.rs always emits MUR_EMBEDDED_AGENT_PATH so the conditional
    // include_bytes! in bin_embed has a valid target even in default builds.
    // The constant is cfg-gated away without the feature; this test only
    // checks that the env var is set at compile time (build.rs ran).
    assert!(
        option_env!("MUR_EMBEDDED_AGENT_PATH").is_some(),
        "MUR_EMBEDDED_AGENT_PATH must be emitted by build.rs"
    );
}
