use crate::identity::AgentIdentity;
use crate::muragent::dsse::{DsseEnvelope, sign as dsse_sign, verify as dsse_verify};
use crate::muragent::MuragentError;
use crate::skill::manifest::SkillManifest;
use crate::skill::scan::scan_unicode;
use crate::skill::serialize_canonical;

pub const SKILL_PAYLOAD_TYPE: &str = "application/vnd.mur.skill+yaml";

#[derive(Debug)]
pub enum SignError {
    Parse(crate::skill::ParseError),
    Muragent(MuragentError),
}

impl std::fmt::Display for SignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignError::Parse(e) => write!(f, "parse: {e}"),
            SignError::Muragent(e) => write!(f, "sign: {e}"),
        }
    }
}

impl std::error::Error for SignError {}

impl From<crate::skill::ParseError> for SignError {
    fn from(e: crate::skill::ParseError) -> Self {
        SignError::Parse(e)
    }
}

impl From<MuragentError> for SignError {
    fn from(e: MuragentError) -> Self {
        SignError::Muragent(e)
    }
}

pub fn sign_manifest(m: &SkillManifest, identity: &AgentIdentity) -> Result<String, SignError> {
    let yaml = serialize_canonical(m)?;
    let (normalised, _) = scan_unicode(&yaml);
    let envelope = dsse_sign(SKILL_PAYLOAD_TYPE, &normalised, identity)?;
    let s =
        serde_json::to_string(&envelope).map_err(|e| MuragentError::Other(format!("envelope json: {e}")))?;
    Ok(s)
}

pub fn verify_manifest(m: &SkillManifest, envelope_json: &str) -> Result<(), SignError> {
    let envelope: DsseEnvelope = serde_json::from_str(envelope_json)
        .map_err(|e| MuragentError::Other(format!("envelope parse: {e}")))?;

    dsse_verify(&envelope, SKILL_PAYLOAD_TYPE)?;

    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    let signed_bytes = B64
        .decode(&envelope.payload)
        .map_err(|e| MuragentError::Other(format!("payload base64: {e}")))?;
    let signed_str = String::from_utf8(signed_bytes)
        .map_err(|e| MuragentError::Other(format!("payload utf8: {e}")))?;

    let yaml = serialize_canonical(m)?;
    let (normalised, _) = scan_unicode(&yaml);
    if signed_str != normalised {
        return Err(SignError::Muragent(MuragentError::InvalidSignature(
            "manifest content does not match signed payload".into(),
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::parse_canonical;

    fn sample() -> SkillManifest {
        let yaml = r#"
name: signed
version: 1.0.0
publisher: human:t
description: d
category: context
content:
  abstract: a
  context: b
"#;
        parse_canonical(yaml).unwrap()
    }

    #[test]
    fn sign_then_verify() {
        let id = AgentIdentity::generate();
        let m = sample();
        let env = sign_manifest(&m, &id).unwrap();
        verify_manifest(&m, &env).unwrap();
    }

    #[test]
    fn tampered_manifest_fails_verify() {
        let id = AgentIdentity::generate();
        let m = sample();
        let env = sign_manifest(&m, &id).unwrap();
        let mut tampered = m.clone();
        tampered.description = "evil".into();
        assert!(verify_manifest(&tampered, &env).is_err());
    }

    #[test]
    fn wrong_payload_type_rejected() {
        let id = AgentIdentity::generate();
        let m = sample();
        let env = sign_manifest(&m, &id).unwrap();
        let mut e: DsseEnvelope = serde_json::from_str(&env).unwrap();
        e.payload_type = "application/vnd.in-toto+json".into();
        let bad = serde_json::to_string(&e).unwrap();
        assert!(verify_manifest(&m, &bad).is_err());
    }
}
