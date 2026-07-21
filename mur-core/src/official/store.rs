//! On-disk store for official catalog licenses (`~/.mur/licenses/`).
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_common::official::{LicenseCheck, OfficialLicense, check_license};
use mur_common::skill::publisher_trust::MUR_OFFICIAL_PUBLISHER_KEY_FP;

pub fn licenses_dir(mur_home: &Path) -> PathBuf {
    mur_home.join("licenses")
}

/// One file per item; `/` flattened so the dir stays flat: `agents__researcher.yaml`.
pub fn license_path(mur_home: &Path, item: &str) -> PathBuf {
    licenses_dir(mur_home).join(format!("{}.yaml", item.replace('/', "__")))
}

/// Atomic write: temp file in the same dir + rename (same pattern as store/yaml.rs).
pub fn save_license(mur_home: &Path, l: &OfficialLicense) -> Result<PathBuf> {
    let dir = licenses_dir(mur_home);
    std::fs::create_dir_all(&dir).context("create licenses dir")?;
    let path = license_path(mur_home, &l.item);
    let yaml = serde_yaml_ng::to_string(l).context("serialize license")?;
    let tmp = tempfile::NamedTempFile::new_in(&dir).context("temp file")?;
    std::fs::write(tmp.path(), yaml).context("write license")?;
    tmp.persist(&path).context("persist license")?;
    Ok(path)
}

pub fn load_license(mur_home: &Path, item: &str) -> Result<Option<OfficialLicense>> {
    let path = license_path(mur_home, item);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("read license {}", path.display()))?;
    Ok(Some(
        serde_yaml_ng::from_str(&text).context("parse license")?,
    ))
}

/// Fail-closed gate used by the import paths. Expiry deliberately NOT checked:
/// a lapsed subscription never disables what the user already obtained.
#[allow(dead_code)]
pub fn require_license(mur_home: &Path, item: &str, user_id: &str) -> Result<()> {
    require_license_against(mur_home, item, user_id, MUR_OFFICIAL_PUBLISHER_KEY_FP)
}

/// Test seam: `require_license` with an explicit official fingerprint, so
/// tests can pin against a locally-generated signing key instead of the real
/// `MUR_OFFICIAL_PUBLISHER_KEY_FP` (which test keys can never match).
pub fn require_license_against(
    mur_home: &Path,
    item: &str,
    user_id: &str,
    official_fp: &str,
) -> Result<()> {
    let Some(l) = load_license(mur_home, item)? else {
        bail!("no license for {item} on this machine");
    };
    match check_license(&l, item, user_id, official_fp) {
        LicenseCheck::Ok => Ok(()),
        LicenseCheck::BadSignature => bail!("license for {item} has an invalid signature"),
        LicenseCheck::NotOfficialKey => {
            bail!("license for {item} is not signed by the MUR official key")
        }
        LicenseCheck::WrongUser => {
            bail!("license for {item} belongs to a different account")
        }
        LicenseCheck::WrongItem => bail!("license file mismatch for {item}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use mur_common::official::{OFFICIAL_LICENSE_FORMAT, OfficialLicense, sign_license};

    fn signed(item: &str, user: &str) -> OfficialLicense {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let mut l = OfficialLicense {
            format_version: OFFICIAL_LICENSE_FORMAT,
            user_id: user.into(),
            item: item.into(),
            version: "1.0.0".into(),
            expires_at: "2027-01-01T00:00:00Z".into(),
            signer_pubkey: String::new(),
            sig: None,
        };
        sign_license(&mut l, &key);
        l
    }

    #[test]
    fn save_load_roundtrip() {
        let home = tempfile::tempdir().unwrap();
        let l = signed("fleets/deep-research", "u1");
        save_license(home.path(), &l).unwrap();
        let got = load_license(home.path(), "fleets/deep-research")
            .unwrap()
            .unwrap();
        assert_eq!(got, l);
    }

    #[test]
    fn load_missing_is_none() {
        let home = tempfile::tempdir().unwrap();
        assert!(load_license(home.path(), "agents/none").unwrap().is_none());
    }

    #[test]
    fn require_license_missing_and_untrusted_signer_fail() {
        let home = tempfile::tempdir().unwrap();
        let err = require_license(home.path(), "fleets/x", "u1").unwrap_err();
        assert!(err.to_string().contains("no license"), "{err}");
        // Present but signed by a non-official key → NotOfficialKey path.
        let l = signed("fleets/x", "u1");
        save_license(home.path(), &l).unwrap();
        let err = require_license(home.path(), "fleets/x", "u1").unwrap_err();
        assert!(
            err.to_string()
                .contains("not signed by the MUR official key"),
            "{err}"
        );
    }

    #[test]
    fn license_path_flattens_slash() {
        let p = license_path(std::path::Path::new("/h"), "agents/researcher");
        assert_eq!(
            p,
            std::path::PathBuf::from("/h/licenses/agents__researcher.yaml")
        );
    }
}
