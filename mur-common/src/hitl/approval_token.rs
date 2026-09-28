//! The HITL approval token — what makes an in-chat `allow` a human's.
//!
//! `tool/hitl_respond` arrives on the agent's own unix socket, and the agent —
//! plus every process it spawns — can dial that socket too. A deny is harmless
//! from anyone; an `allow: true` is honoured only when it carries this token.
//!
//! The token lives at `<mur_home>/secrets/hitl-approval-token`. `secrets/` is
//! denied read AND write to every sealed agent (`mur-agent-runtime`,
//! `sandbox/launch_chain.rs`, `protects_credential`), so the Hub and
//! `mur agent cli` can read it and the agent cannot. The runtime reads it
//! before it seals itself, the same way it resolves provider credentials.
//!
//! One value for the whole home, created on first use and never rotated
//! automatically: a sealed agent cannot learn it, and an agent whose sandbox is
//! not enforcing has every allow refused anyway, token or not. To rotate it,
//! delete the file and restart the agents.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Directory under the MUR home that sealed agents can neither read nor write.
pub const SECRETS_DIR: &str = "secrets";
/// File name of the token inside [`SECRETS_DIR`].
pub const FILE_NAME: &str = "hitl-approval-token";
/// The `tool/hitl_respond` param that carries the token.
pub const PARAM: &str = "approval_token";
/// Random bytes in a token. Stored hex-encoded, so the file holds twice this.
const TOKEN_BYTES: usize = 32;

/// Where the token lives for this MUR home.
pub fn path(mur_home: &Path) -> PathBuf {
    mur_home.join(SECRETS_DIR).join(FILE_NAME)
}

/// Read the token, or `Ok(None)` when nothing has created it yet.
///
/// Errors when the file exists but cannot be read, or holds something that is
/// not a token. An empty or short value is never accepted: an empty token would
/// match an empty param, which is the same as having no check at all.
pub fn read(mur_home: &Path) -> io::Result<Option<String>> {
    let p = path(mur_home);
    match std::fs::read_to_string(&p) {
        Ok(s) => parse(&s, &p).map(Some),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Read the token, creating it first if it does not exist yet.
///
/// Creation writes a private temp file in `secrets/` and renames it into place
/// without overwriting, so a reader never sees a half-written token and two
/// processes racing to create it both end up holding the winner's value.
pub fn load_or_create(mur_home: &Path) -> io::Result<String> {
    if let Some(t) = read(mur_home)? {
        return Ok(t);
    }
    let dir = mur_home.join(SECRETS_DIR);
    create_private_dir(&dir)?;
    let token = generate();
    // `NamedTempFile` is created 0600 on unix.
    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    tmp.write_all(token.as_bytes())?;
    tmp.as_file().sync_all()?;
    match tmp.persist_noclobber(path(mur_home)) {
        Ok(_) => Ok(token),
        // Someone else created it between our read and our rename: theirs wins.
        Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => read(mur_home)?
            .ok_or_else(|| io::Error::other("approval token vanished while being created")),
        Err(e) => Err(e.error),
    }
}

/// Put the token on `tool/hitl_respond` params that approve.
///
/// A deny is left alone: the runtime accepts a deny from anyone, so sending the
/// secret with one would only widen where it travels. When no token exists yet
/// the params go out without one and the runtime's refusal says why.
pub fn attach(params: &mut Value, mur_home: &Path) -> io::Result<()> {
    if params.get("allow").and_then(Value::as_bool) != Some(true) {
        return Ok(());
    }
    if let Some(t) = read(mur_home)?
        && let Some(obj) = params.as_object_mut()
    {
        obj.insert(PARAM.to_string(), Value::String(t));
    }
    Ok(())
}

fn generate() -> String {
    use rand_core::RngCore;
    let mut bytes = [0u8; TOKEN_BYTES];
    rand_core::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn parse(raw: &str, p: &Path) -> io::Result<String> {
    let t = raw.trim();
    if t.len() == TOKEN_BYTES * 2 && t.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(t.to_string())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is not a valid approval token; delete it and restart the agents to make a new one",
                p.display()
            ),
        ))
    }
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        // Applies only to directories this call creates; an existing
        // `secrets/` keeps the mode the user gave it.
        b.mode(0o700);
    }
    b.create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn creates_once_then_returns_the_same_token() {
        let home = tempfile::tempdir().unwrap();
        assert!(read(home.path()).unwrap().is_none());
        let a = load_or_create(home.path()).unwrap();
        assert_eq!(a.len(), TOKEN_BYTES * 2);
        assert_eq!(load_or_create(home.path()).unwrap(), a);
        assert_eq!(read(home.path()).unwrap().as_deref(), Some(a.as_str()));
    }

    #[test]
    fn two_homes_get_different_tokens() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        assert_ne!(
            load_or_create(a.path()).unwrap(),
            load_or_create(b.path()).unwrap()
        );
    }

    #[test]
    fn racing_creators_agree_on_one_token() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path().to_path_buf();
        let got: Vec<String> = (0..8)
            .map(|_| {
                let h = h.clone();
                std::thread::spawn(move || load_or_create(&h).unwrap())
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|j| j.join().unwrap())
            .collect();
        assert!(got.iter().all(|t| t == &got[0]), "{got:?}");
        assert_eq!(read(&h).unwrap().as_ref(), Some(&got[0]));
    }

    #[test]
    fn a_malformed_file_is_an_error_not_a_token() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(SECRETS_DIR)).unwrap();
        for bad in ["", "   \n", "short", &"z".repeat(TOKEN_BYTES * 2)] {
            std::fs::write(path(home.path()), bad).unwrap();
            let e = read(home.path()).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{bad:?}");
            // And it is never silently replaced.
            assert!(load_or_create(home.path()).is_err(), "{bad:?}");
            assert_eq!(std::fs::read_to_string(path(home.path())).unwrap(), bad);
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_token_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        load_or_create(home.path()).unwrap();
        let mode = std::fs::metadata(path(home.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    }

    #[test]
    fn only_an_allow_carries_the_token() {
        let home = tempfile::tempdir().unwrap();
        let token = load_or_create(home.path()).unwrap();

        let mut allow = json!({ "hitl_id": "h", "allow": true, "surface": "hub" });
        attach(&mut allow, home.path()).unwrap();
        assert_eq!(allow[PARAM], token.as_str());

        let mut deny = json!({ "hitl_id": "h", "allow": false, "surface": "hub" });
        attach(&mut deny, home.path()).unwrap();
        assert!(
            deny.get(PARAM).is_none(),
            "a deny must not carry the secret"
        );
    }

    #[test]
    fn no_token_yet_sends_the_allow_without_one() {
        let home = tempfile::tempdir().unwrap();
        let mut allow = json!({ "hitl_id": "h", "allow": true });
        attach(&mut allow, home.path()).unwrap();
        assert!(allow.get(PARAM).is_none());
        // The sender must not create it: that is the runtime's job, pre-seal.
        assert!(!path(home.path()).exists());
    }
}
