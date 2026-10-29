//! Key fingerprints and `session-bind` signature verification.
//!
//! The host key in a `session-bind` is client-asserted until its signature
//! over the session identifier verifies; only then may it be matched against
//! policy. Verifiers come from `ring` (spec §"`session-bind` verification").
//! Every algorithm not in that table fails closed.

use base64::Engine as _;
use ring::signature::{self, UnparsedPublicKey};
use sha2::{Digest, Sha256};

use super::wire::Reader;

/// OpenSSH-format fingerprint: `SHA256:` + unpadded base64 of the blob hash.
pub fn fingerprint(key_blob: &[u8]) -> String {
    let digest = Sha256::digest(key_blob);
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
    )
}

/// Why a bind signature was not accepted. Every variant maps to the single
/// refusal `session_bind_unverified`; the variant is for the log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindError {
    /// Key or signature blob does not parse.
    Malformed,
    /// Algorithm outside the supported table (P-521, SHA-1 `ssh-rsa`,
    /// certificates, anything unknown). Carries the name for the log.
    Unsupported(String),
    /// Signature algorithm does not belong to the host-key type.
    TypeMismatch { key: String, sig: String },
    /// Parsed fine, but the signature is wrong.
    BadSignature,
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed => write!(f, "host key or signature does not parse"),
            Self::Unsupported(a) => write!(f, "unsupported algorithm {a}"),
            Self::TypeMismatch { key, sig } => {
                write!(f, "signature type {sig} does not match host key type {key}")
            }
            Self::BadSignature => write!(f, "signature does not verify"),
        }
    }
}

/// Verify that `host_key` signed `session_id`, as OpenSSH's own agent does
/// for `session-bind@openssh.com`.
pub fn verify_session_bind(
    host_key: &[u8],
    session_id: &[u8],
    sig_blob: &[u8],
) -> Result<(), BindError> {
    let malformed = |_| BindError::Malformed;
    let mut k = Reader::new(host_key);
    let key_type = utf8(k.string().map_err(malformed)?)?;
    let mut s = Reader::new(sig_blob);
    let sig_type = utf8(s.string().map_err(malformed)?)?;
    let sig = s.string().map_err(malformed)?;
    s.finish().map_err(malformed)?;

    let mismatch = || BindError::TypeMismatch {
        key: key_type.clone(),
        sig: sig_type.clone(),
    };
    match key_type.as_str() {
        "ssh-ed25519" => {
            if sig_type != "ssh-ed25519" {
                return Err(mismatch());
            }
            let pk = k.string().map_err(malformed)?;
            k.finish().map_err(malformed)?;
            check(&signature::ED25519, pk, session_id, sig)
        }
        "ecdsa-sha2-nistp256" | "ecdsa-sha2-nistp384" => {
            if sig_type != key_type {
                return Err(mismatch());
            }
            let (alg, curve, width): (&'static dyn signature::VerificationAlgorithm, _, _) =
                if key_type.ends_with("256") {
                    (&signature::ECDSA_P256_SHA256_FIXED, "nistp256", 32)
                } else {
                    (&signature::ECDSA_P384_SHA384_FIXED, "nistp384", 48)
                };
            if k.string().map_err(malformed)? != curve.as_bytes() {
                return Err(BindError::Malformed);
            }
            let point = k.string().map_err(malformed)?;
            k.finish().map_err(malformed)?;
            let fixed = ecdsa_fixed(sig, width)?;
            check(alg, point, session_id, &fixed)
        }
        "ssh-rsa" => {
            let params = match sig_type.as_str() {
                "rsa-sha2-256" => &signature::RSA_PKCS1_2048_8192_SHA256,
                "rsa-sha2-512" => &signature::RSA_PKCS1_2048_8192_SHA512,
                // SHA-1 `ssh-rsa` signatures are refused, not verified.
                "ssh-rsa" => return Err(BindError::Unsupported(sig_type)),
                _ => return Err(mismatch()),
            };
            let e = strip_mpint(k.string().map_err(malformed)?);
            let n = strip_mpint(k.string().map_err(malformed)?);
            k.finish().map_err(malformed)?;
            signature::RsaPublicKeyComponents { n, e }
                .verify(params, session_id, sig)
                .map_err(|_| BindError::BadSignature)
        }
        _ => Err(BindError::Unsupported(key_type)),
    }
}

fn utf8(b: &[u8]) -> Result<String, BindError> {
    String::from_utf8(b.to_vec()).map_err(|_| BindError::Malformed)
}

fn check(
    alg: &'static dyn signature::VerificationAlgorithm,
    pk: &[u8],
    msg: &[u8],
    sig: &[u8],
) -> Result<(), BindError> {
    UnparsedPublicKey::new(alg, pk)
        .verify(msg, sig)
        .map_err(|_| BindError::BadSignature)
}

/// An SSH `mpint` is two's complement with a leading zero byte when the high
/// bit is set. Positive values only here, so stripping leading zeros yields
/// the big-endian magnitude `ring` expects.
fn strip_mpint(b: &[u8]) -> &[u8] {
    let i = b.iter().position(|&x| x != 0).unwrap_or(b.len());
    &b[i..]
}

/// SSH ECDSA signature `string(mpint r || mpint s)` → fixed `r || s`, each
/// left-padded to the curve width.
fn ecdsa_fixed(sig: &[u8], width: usize) -> Result<Vec<u8>, BindError> {
    let mut r = Reader::new(sig);
    let a = strip_mpint(r.string().map_err(|_| BindError::Malformed)?);
    let b = strip_mpint(r.string().map_err(|_| BindError::Malformed)?);
    r.finish().map_err(|_| BindError::Malformed)?;
    if a.len() > width || b.len() > width {
        return Err(BindError::Malformed);
    }
    let mut out = vec![0u8; width * 2];
    out[width - a.len()..width].copy_from_slice(a);
    out[2 * width - b.len()..].copy_from_slice(b);
    Ok(out)
}
