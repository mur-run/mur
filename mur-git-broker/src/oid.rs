use crate::constants::*;
use crate::error::BrokerError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectFormat {
    Sha1,
    Sha256,
}
impl ObjectFormat {
    pub fn hex_len(self) -> usize {
        match self {
            Self::Sha1 => SHA1_HEX_LEN,
            Self::Sha256 => SHA256_HEX_LEN,
        }
    }
    pub fn as_git_arg(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
        }
    }
}
pub fn zero_oid(f: ObjectFormat) -> String {
    "0".repeat(f.hex_len())
}
pub fn validate_oid(s: &str, f: ObjectFormat) -> Result<(), BrokerError> {
    if s.len() == f.hex_len() && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(())
    } else {
        Err(BrokerError::InvalidRequest("oid".into()))
    }
}
pub fn validate_ref(r: &str) -> Result<(), BrokerError> {
    let tail = r.strip_prefix(ALLOWED_REF_PREFIX).filter(|t| !t.is_empty());
    let ok = tail.is_some_and(|t| {
        !t.contains("..")
            && !t.contains("//")
            && !t.ends_with('/')
            && !t.ends_with(".lock")
            && !t.starts_with('.')
            && t.bytes()
                .all(|b| b.is_ascii_graphic() && !b"~^:?*[\\".contains(&b))
    });
    if ok {
        Ok(())
    } else {
        Err(BrokerError::InvalidRequest("ref".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zero_oid_lengths() {
        assert_eq!(zero_oid(ObjectFormat::Sha1), "0".repeat(40));
        assert_eq!(zero_oid(ObjectFormat::Sha256), "0".repeat(64));
    }
    #[test]
    fn rejects_short_upper_and_null_spellings() {
        for bad in [
            "",
            "null",
            "abc",
            &"A".repeat(40),
            &"g".repeat(40),
            &"a".repeat(39),
        ] {
            assert!(validate_oid(bad, ObjectFormat::Sha1).is_err(), "{bad}");
        }
        assert!(validate_oid(&"a".repeat(40), ObjectFormat::Sha1).is_ok());
        assert!(validate_oid(&"a".repeat(40), ObjectFormat::Sha256).is_err());
    }
    #[test]
    fn ref_must_be_full_and_clean() {
        for bad in [
            "main",
            "refs/tags/v1",
            "refs/heads/agent/../x",
            "refs/heads/agent/a b",
            "refs/heads/agent/",
            "refs/heads/agent/x.lock",
            "refs/heads/agent/x\n",
            "refs/heads//x",
        ] {
            assert!(validate_ref(bad).is_err(), "{bad:?}");
        }
        assert!(validate_ref("refs/heads/agent/task-123").is_ok());
    }
}
