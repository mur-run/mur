//! OpenSSH `SHA256:` fingerprints, for the CLI that writes `entitlements.ssh`.
//!
//! The runtime matches on these strings and nothing else, so the CLI must
//! produce exactly the form `ssh-keygen -lf` prints: `SHA256:` followed by the
//! unpadded base64 of the SHA-256 of the key blob.

use base64::Engine as _;
use sha2::{Digest, Sha256};

/// Prefix of every fingerprint the policy accepts.
pub const FINGERPRINT_PREFIX: &str = "SHA256:";

/// Length of the unpadded base64 of a 32-byte digest.
const DIGEST_B64_LEN: usize = 43;

/// Fingerprint of a raw public key blob.
pub fn fingerprint_of_blob(blob: &[u8]) -> String {
    let digest = Sha256::digest(blob);
    format!(
        "{FINGERPRINT_PREFIX}{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
    )
}

/// Fingerprint of the base64 key field of a `known_hosts` / `ssh-add -L`
/// line. `None` when the field is not valid base64.
pub fn fingerprint_of_base64(key_base64: &str) -> Option<String> {
    base64::engine::general_purpose::STANDARD
        .decode(key_base64)
        .ok()
        .map(|blob| fingerprint_of_blob(&blob))
}

/// Whether `s` is a well-formed fingerprint (the same rule the proxy policy
/// applies; a malformed entry makes the whole policy refuse).
pub fn is_fingerprint(s: &str) -> bool {
    s.strip_prefix(FINGERPRINT_PREFIX).is_some_and(|b| {
        b.len() == DIGEST_B64_LEN
            && base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(b)
                .is_ok_and(|d| d.len() == 32)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ssh-keygen -lf` on this ed25519 key prints exactly this fingerprint.
    const ED25519_B64: &str =
        "AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";
    const ED25519_FP: &str = "SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU";

    #[test]
    fn matches_ssh_keygen_output() {
        assert_eq!(
            fingerprint_of_base64(ED25519_B64).as_deref(),
            Some(ED25519_FP)
        );
        assert!(is_fingerprint(ED25519_FP));
    }

    #[test]
    fn rejects_malformed_fingerprints_and_base64() {
        for bad in [
            "SHA256:short",
            "MD5:aa",
            ED25519_FP.trim_start_matches("SHA256:"),
            "",
        ] {
            assert!(!is_fingerprint(bad), "{bad}");
        }
        assert_eq!(fingerprint_of_base64("not base64!"), None);
    }
}
