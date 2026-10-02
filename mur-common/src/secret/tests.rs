/// A command line is exactly where an inline credential lives, and pattern
/// redaction does not cover it: `redact_secrets` matches known key shapes
/// and `--token=anything` is none of them. The arguments go structurally.
#[test]
fn a_command_reference_hides_its_arguments() {
    let r = SecretRef::Cmd("vault read -field=key secret/x --token=orgtok123".into());
    let label = r.label();
    assert!(label.starts_with("cmd:vault"), "{label}");
    assert!(!label.contains("orgtok123"), "{label}");
    assert!(label.contains("arguments hidden"), "{label}");
}

/// …but a bare command has nothing to hide, and saying "arguments hidden"
/// when there are none is its own small lie.
#[test]
fn a_bare_command_reference_is_shown_whole() {
    assert_eq!(SecretRef::Cmd("get-key".into()).label(), "cmd:get-key");
}

/// The other forms name a location, not a payload, so they are unchanged —
/// `mur agent doctor` has printed them for a long time.
#[test]
fn the_other_forms_are_unchanged() {
    for r in [
        SecretRef::Env("ANTHROPIC_API_KEY".into()),
        SecretRef::File("/home/d/.mur/secrets/k".into()),
        SecretRef::Keychain {
            service: "mur".into(),
            account: "anthropic".into(),
        },
    ] {
        assert_eq!(r.label(), r.to_string(), "{r}");
    }
}

use super::*;
use serde_yaml_ng as yaml;

#[test]
fn parses_env_form() {
    let s: SecretRef = yaml::from_str("env:ANTHROPIC_API_KEY").unwrap();
    assert_eq!(s, SecretRef::Env("ANTHROPIC_API_KEY".into()));
}

#[test]
fn parses_keychain_form() {
    let s: SecretRef = yaml::from_str("keychain:mur/anthropic-oauth").unwrap();
    assert_eq!(
        s,
        SecretRef::Keychain {
            service: "mur".into(),
            account: "anthropic-oauth".into()
        }
    );
}

#[test]
fn parses_file_form() {
    let s: SecretRef = yaml::from_str("file:/tmp/foo.age").unwrap();
    assert_eq!(s, SecretRef::File(PathBuf::from("/tmp/foo.age")));
}

#[test]
fn parses_cmd_form() {
    let s: SecretRef = yaml::from_str("cmd:op read op://vault/item/field").unwrap();
    assert_eq!(s, SecretRef::Cmd("op read op://vault/item/field".into()));
}

#[test]
fn rejects_unknown_scheme() {
    let r: Result<SecretRef, _> = yaml::from_str("plain:supersecret");
    assert!(r.is_err());
}

#[test]
fn round_trip_serde() {
    let cases = [
        "env:X",
        "keychain:svc/acct",
        "file:/p",
        "cmd:bin --flag arg",
    ];
    for s in cases {
        let parsed: SecretRef = yaml::from_str(s).unwrap();
        let back = yaml::to_string(&parsed).unwrap();
        // serde-yaml adds a trailing newline / quoting. Strip and compare.
        let normalized = back
            .trim()
            .trim_matches(|c: char| c == '"' || c == '\'')
            .to_string();
        let reparsed: SecretRef = yaml::from_str(&normalized).unwrap();
        assert_eq!(parsed, reparsed, "round-trip drift for {s}");
    }
}
