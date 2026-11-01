use super::*;

const ED25519_B64: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";
const ED25519_FP: &str = "SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU";

#[test]
fn keyscan_output_parses_skipping_comments_garbage_and_duplicates() {
    let out = format!(
        "# github.com:22 SSH-2.0-x\n\
         github.com ssh-ed25519 {ED25519_B64}\n\
         github.com ssh-ed25519 {ED25519_B64}\n\
         github.com ssh-rsa !!notbase64!!\n\
         short line\n"
    );
    let keys = parse_keyscan(&out);
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].fingerprint, ED25519_FP);
    assert_eq!(keys[0].entry.host, "github.com");
    assert_eq!(keys[0].entry.key_type, "ssh-ed25519");
}

#[test]
fn hosts_that_could_be_options_or_lists_are_rejected() {
    for bad in ["", "-oProxyCommand=x", "a b", "a,b", "a\nb"] {
        assert!(validate_host(bad).is_err(), "{bad:?}");
    }
    assert!(validate_host("people.example.edu").is_ok());
}

#[test]
fn comment_lookup_needs_exactly_one_match_and_returns_only_the_fingerprint() {
    let listed = format!("ssh-ed25519 {ED25519_B64} deploy@ci\nssh-ed25519 {ED25519_B64} other\n");
    assert_eq!(
        match_comment(&listed, "deploy@ci").unwrap(),
        KeyRef::Resolved {
            fingerprint: ED25519_FP.into(),
            comment: "deploy@ci".into()
        }
    );
    assert!(match_comment(&listed, "missing").is_err());
    let dup = format!("ssh-ed25519 {ED25519_B64} same\nssh-ed25519 {ED25519_B64} same\n");
    assert!(match_comment(&dup, "same").is_err());
}

#[test]
fn a_fingerprint_is_taken_as_is_and_a_malformed_one_is_refused() {
    assert_eq!(
        resolve_key(ED25519_FP).unwrap(),
        KeyRef::Fingerprint(ED25519_FP.into())
    );
    assert!(resolve_key("SHA256:short").is_err());
}
