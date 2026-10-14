//! Where a credential's secret is read from (credentials design §2).
//!
//! `CredentialSource::open` is the only place the filesystem is touched for a credential, and it
//! runs the checks in a fixed order. The order is the point: ownership of the path is decided on
//! the *resolved* path, and the file facts (type, mode, owner, size) are read from the opened
//! descriptor, not from the path again, so nothing can be swapped between check and read.

use crate::constants::*;
use crate::credential::SecretPath;
use crate::error::BrokerError;
use secrecy::SecretString;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// Hands out the secret a credential points at. Implementations never log it and never put it in
/// an error: every failure is a fixed reason token, with no path and no file content.
pub trait CredentialSource: Send + Sync {
    fn open(&self, path: &SecretPath) -> Result<SecretString, BrokerError>;
}

/// Reads secrets from regular files under one directory (normally `~/.mur/secrets/`).
#[derive(Debug, Clone)]
pub struct FileCredentials {
    secrets_root: PathBuf,
    owner_uid: u32,
}

impl FileCredentials {
    /// `secrets_root` is the directory every credential must resolve under. Files must be owned
    /// by the effective uid of this process.
    pub fn new(secrets_root: impl Into<PathBuf>) -> Self {
        // SAFETY: geteuid has no preconditions and cannot fail.
        let owner_uid = unsafe { libc::geteuid() };
        Self {
            secrets_root: secrets_root.into(),
            owner_uid,
        }
    }
}

/// Refusal reasons. Fixed tokens: they reach logs and the wire, so they carry no path and no
/// file content.
mod why {
    pub const OUTSIDE: &str = "outside_secrets";
    pub const NOT_FOUND: &str = "not_found";
    pub const UNREADABLE: &str = "unreadable";
    pub const NOT_REGULAR: &str = "not_regular";
    pub const BAD_MODE: &str = "bad_mode";
    pub const BAD_OWNER: &str = "bad_owner";
    pub const TOO_LARGE: &str = "too_large";
    pub const EMPTY: &str = "empty";
    pub const BAD_CONTENT: &str = "bad_content";
}

fn refuse(reason: &'static str) -> BrokerError {
    BrokerError::CredentialUnreadable(reason)
}

fn resolve(p: &Path) -> Result<PathBuf, &'static str> {
    p.canonicalize().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => why::NOT_FOUND,
        _ => why::UNREADABLE,
    })
}

/// The real path of an open descriptor, from the kernel rather than from any path we hold.
#[cfg(target_os = "macos")]
fn descriptor_path(file: &std::fs::File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    let mut buf = [0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes a NUL-terminated path of at most PATH_MAX bytes into `buf`, which
    // is exactly that long, and the descriptor is open for the duration of the call.
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) };
    if rc == -1 {
        return None;
    }
    let len = buf.iter().position(|&b| b == 0)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len])))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn descriptor_path(file: &std::fs::File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()
}

/// No way to ask a descriptor for its path here, so nothing can be confirmed: refuse.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
fn descriptor_path(_file: &std::fs::File) -> Option<PathBuf> {
    None
}

/// Open `real` (already resolved) and confirm, from the descriptor, that it is under `root`.
/// Fails closed: a descriptor whose path cannot be read is refused.
fn open_confined(root: &Path, real: &Path) -> Result<std::fs::File, BrokerError> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(real)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => refuse(why::NOT_FOUND),
            _ => refuse(why::UNREADABLE),
        })?;
    match descriptor_path(&file) {
        Some(actual) if actual.starts_with(root) => Ok(file),
        _ => Err(refuse(why::OUTSIDE)),
    }
}

impl CredentialSource for FileCredentials {
    /// Order matters, and each step relies on the one before:
    /// 1. resolve the root and the path (`realpath`), so every symlink is gone before any
    ///    comparison;
    /// 2. the resolved path is under the resolved root (component-wise, so `secrets-evil` is not
    ///    under `secrets`);
    /// 3. open the resolved path without following a final symlink and without blocking, so a
    ///    FIFO cannot hang the daemon, then ask the descriptor where it really is and require
    ///    that to be under the root too: `O_NOFOLLOW` covers only the last component, so a parent
    ///    directory swapped for a symlink after step 2 is caught here;
    /// 4. from here on every fact comes from the open descriptor, never the path again: regular
    ///    file, mode exactly `0600`, owned by this uid, size within the bound;
    /// 5. read at most the bound plus one byte, once, into a buffer that is wiped on drop;
    /// 6. UTF-8, one trailing newline trimmed, not empty, no control characters.
    fn open(&self, path: &SecretPath) -> Result<SecretString, BrokerError> {
        let root = resolve(&self.secrets_root).map_err(|_| refuse(why::OUTSIDE))?;
        let real = resolve(Path::new(path.as_str())).map_err(refuse)?;
        if !real.starts_with(&root) {
            return Err(refuse(why::OUTSIDE));
        }

        let file = open_confined(&root, &real)?;
        let meta = file.metadata().map_err(|_| refuse(why::UNREADABLE))?;
        if !meta.is_file() {
            return Err(refuse(why::NOT_REGULAR));
        }
        // All of 07777: a stray special bit is not part of a secret's contract either.
        if meta.mode() & 0o7777 != CREDENTIAL_FILE_MODE {
            return Err(refuse(why::BAD_MODE));
        }
        if meta.uid() != self.owner_uid {
            return Err(refuse(why::BAD_OWNER));
        }
        if meta.len() > CREDENTIAL_MAX_BYTES {
            return Err(refuse(why::TOO_LARGE));
        }

        let mut buf = Zeroizing::new(Vec::with_capacity(meta.len() as usize + 1));
        file.take(CREDENTIAL_MAX_BYTES + 1)
            .read_to_end(&mut buf)
            .map_err(|_| refuse(why::UNREADABLE))?;
        if buf.len() as u64 > CREDENTIAL_MAX_BYTES {
            return Err(refuse(why::TOO_LARGE)); // grew after the size check
        }

        let text = std::str::from_utf8(&buf).map_err(|_| refuse(why::BAD_CONTENT))?;
        let text = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .unwrap_or(text);
        if text.is_empty() {
            return Err(refuse(why::EMPTY));
        }
        if text.chars().any(char::is_control) {
            return Err(refuse(why::BAD_CONTENT));
        }
        Ok(SecretString::from(text.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use tempfile::TempDir;

    struct Fx {
        _tmp: TempDir,
        root: PathBuf,
        outside: PathBuf,
    }

    fn fx() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("secrets");
        let outside = tmp.path().join("elsewhere");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        Fx {
            _tmp: tmp,
            root,
            outside,
        }
    }

    fn put(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    fn sp(p: &Path) -> SecretPath {
        SecretPath::parse(p.to_str().unwrap()).unwrap()
    }

    fn reason(r: Result<SecretString, BrokerError>) -> &'static str {
        match r {
            Err(BrokerError::CredentialUnreadable(why)) => why,
            Err(other) => panic!("wrong error: {other:?}"),
            Ok(_) => panic!("expected a refusal, got a secret"),
        }
    }

    fn src(f: &Fx) -> FileCredentials {
        FileCredentials::new(&f.root)
    }

    #[test]
    fn reads_a_0600_file_and_trims_one_trailing_newline() {
        let f = fx();
        let p = put(&f.root, "tok", b"ghp_abc123\n", 0o600);
        let s = src(&f).open(&sp(&p)).unwrap();
        assert_eq!(s.expose_secret(), "ghp_abc123");
    }

    #[test]
    fn a_file_at_the_size_limit_is_read() {
        let f = fx();
        let body = vec![b'a'; CREDENTIAL_MAX_BYTES as usize];
        let p = put(&f.root, "tok", &body, 0o600);
        assert_eq!(
            src(&f).open(&sp(&p)).unwrap().expose_secret().len(),
            body.len()
        );
    }

    #[test]
    fn a_file_over_the_size_limit_is_refused() {
        let f = fx();
        let body = vec![b'a'; CREDENTIAL_MAX_BYTES as usize + 1];
        let p = put(&f.root, "tok", &body, 0o600);
        assert_eq!(reason(src(&f).open(&sp(&p))), "too_large");
    }

    #[test]
    fn a_missing_file_is_not_found() {
        let f = fx();
        assert_eq!(reason(src(&f).open(&sp(&f.root.join("nope")))), "not_found");
    }

    #[test]
    fn a_dangling_symlink_is_not_found() {
        let f = fx();
        symlink(f.root.join("gone"), f.root.join("link")).unwrap();
        assert_eq!(reason(src(&f).open(&sp(&f.root.join("link")))), "not_found");
    }

    #[test]
    fn a_missing_secrets_root_is_refused() {
        let f = fx();
        let p = put(&f.root, "tok", b"x", 0o600);
        let s = FileCredentials::new(f.root.join("no-such-dir"));
        assert_eq!(reason(s.open(&sp(&p))), "outside_secrets");
    }

    #[test]
    fn a_path_outside_secrets_is_refused_even_when_well_formed() {
        let f = fx();
        let p = put(&f.outside, "tok", b"x", 0o600);
        assert_eq!(reason(src(&f).open(&sp(&p))), "outside_secrets");
    }

    #[test]
    fn a_sibling_that_shares_the_directory_name_prefix_is_refused() {
        let f = fx();
        let evil = f.root.with_file_name("secrets-evil");
        fs::create_dir(&evil).unwrap();
        let p = put(&evil, "tok", b"x", 0o600);
        assert_eq!(reason(src(&f).open(&sp(&p))), "outside_secrets");
    }

    #[test]
    fn a_symlink_inside_secrets_that_escapes_is_refused() {
        let f = fx();
        let target = put(&f.outside, "tok", b"x", 0o600);
        let link = f.root.join("link");
        symlink(&target, &link).unwrap();
        assert_eq!(reason(src(&f).open(&sp(&link))), "outside_secrets");
    }

    #[test]
    fn a_symlinked_directory_inside_secrets_that_escapes_is_refused() {
        let f = fx();
        put(&f.outside, "tok", b"x", 0o600);
        symlink(&f.outside, f.root.join("dir")).unwrap();
        assert_eq!(
            reason(src(&f).open(&sp(&f.root.join("dir/tok")))),
            "outside_secrets"
        );
    }

    // The race the descriptor check closes: `open` resolves and compares, then opens by path. If a
    // parent directory is swapped for a symlink in between, the path handed to the open no longer
    // resolves where the comparison said. `open_confined` is that second half on its own, so the
    // swapped path can be given to it directly.
    #[test]
    fn a_parent_swapped_for_a_symlink_after_the_root_check_is_refused() {
        let f = fx();
        let root = f.root.canonicalize().unwrap();
        put(&f.outside, "token", b"stolen\n", 0o600);
        symlink(&f.outside, root.join("dir")).unwrap();
        let swapped = root.join("dir").join("token");
        assert_eq!(
            open_confined(&root, &swapped).unwrap_err(),
            refuse(why::OUTSIDE)
        );
    }
    #[test]
    fn a_file_really_under_the_root_is_opened_by_the_confined_open() {
        let f = fx();
        let root = f.root.canonicalize().unwrap();
        let p = put(&root, "token", b"ok\n", 0o600);
        assert!(open_confined(&root, &p).is_ok());
    }
    #[test]
    fn a_symlink_that_stays_inside_secrets_is_followed() {
        let f = fx();
        let target = put(&f.root, "real", b"tok1", 0o600);
        let link = f.root.join("alias");
        symlink(&target, &link).unwrap();
        assert_eq!(src(&f).open(&sp(&link)).unwrap().expose_secret(), "tok1");
    }

    #[test]
    fn a_root_that_is_itself_a_symlink_is_resolved_before_comparing() {
        let f = fx();
        let p = put(&f.root, "tok", b"tok1", 0o600);
        let via = f.outside.join("root-link");
        symlink(&f.root, &via).unwrap();
        let s = FileCredentials::new(&via);
        // The real path and a path spelled through the link both land under the real root.
        assert_eq!(s.open(&sp(&p)).unwrap().expose_secret(), "tok1");
        assert_eq!(
            s.open(&sp(&via.join("tok"))).unwrap().expose_secret(),
            "tok1"
        );
    }

    #[test]
    fn a_directory_is_not_a_regular_file() {
        let f = fx();
        let d = f.root.join("d");
        fs::create_dir(&d).unwrap();
        fs::set_permissions(&d, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(reason(src(&f).open(&sp(&d))), "not_regular");
    }

    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let f = fx();
        let p = f.root.join("fifo");
        let c = std::ffi::CString::new(p.to_str().unwrap()).unwrap();
        // SAFETY: valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        // Run it on a thread: if the open ever blocks on the FIFO, this test must fail, not hang.
        let (tx, rx) = std::sync::mpsc::channel();
        let (source, path) = (src(&f), sp(&p));
        std::thread::spawn(move || {
            let _ = tx.send(source.open(&path));
        });
        let r = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("open blocked on a FIFO");
        assert_eq!(reason(r), "not_regular");
    }

    #[test]
    fn any_mode_other_than_exactly_0600_is_refused() {
        // 0o1600 (sticky) is the special bit an owner can actually leave on a plain file: the
        // kernel clears setuid/setgid there, so those cannot be constructed for this test.
        for mode in [0o644, 0o640, 0o660, 0o604, 0o400, 0o700, 0o1600] {
            let f = fx();
            let p = put(&f.root, "tok", b"x", mode);
            assert_eq!(reason(src(&f).open(&sp(&p))), "bad_mode", "mode {mode:o}");
        }
    }

    #[test]
    fn a_file_owned_by_someone_else_is_refused() {
        let f = fx();
        let p = put(&f.root, "tok", b"x", 0o600);
        let mut s = src(&f);
        s.owner_uid = s.owner_uid.wrapping_add(1);
        assert_eq!(reason(s.open(&sp(&p))), "bad_owner");
    }

    #[test]
    fn an_empty_file_or_a_lone_newline_is_refused() {
        let f = fx();
        for (n, body) in [("a", &b""[..]), ("b", b"\n"), ("c", b"\r\n")] {
            let p = put(&f.root, n, body, 0o600);
            assert_eq!(reason(src(&f).open(&sp(&p))), "empty", "{n}");
        }
    }

    #[test]
    fn bytes_that_are_not_utf8_are_refused() {
        let f = fx();
        let p = put(&f.root, "tok", &[0xff, 0xfe, b'a'], 0o600);
        assert_eq!(reason(src(&f).open(&sp(&p))), "bad_content");
    }

    #[test]
    fn an_interior_newline_or_control_character_is_refused_and_not_echoed() {
        let f = fx();
        for (n, body) in [
            ("a", &b"hunter2\nmore"[..]),
            ("b", b"hunter2\x07"),
            ("c", b"hun\0ter2"),
        ] {
            let p = put(&f.root, n, body, 0o600);
            let e = src(&f).open(&sp(&p)).unwrap_err();
            assert_eq!(e, BrokerError::CredentialUnreadable("bad_content"), "{n}");
            assert!(!format!("{e} {e:?}").contains("hunter2"));
        }
    }

    #[test]
    fn the_secret_does_not_appear_in_debug_output() {
        let f = fx();
        let p = put(&f.root, "tok", b"hunter2", 0o600);
        let s = src(&f).open(&sp(&p)).unwrap();
        assert!(!format!("{s:?}").contains("hunter2"));
    }

    #[test]
    fn the_trait_is_object_safe_and_a_fake_can_stand_in() {
        struct Fake(&'static str);
        impl CredentialSource for Fake {
            fn open(&self, _: &SecretPath) -> Result<SecretString, BrokerError> {
                Ok(SecretString::from(self.0))
            }
        }
        let b: Box<dyn CredentialSource> = Box::new(Fake("fake-token"));
        let p = SecretPath::parse("/anywhere").unwrap();
        assert_eq!(b.open(&p).unwrap().expose_secret(), "fake-token");
    }

    #[test]
    fn the_new_error_has_its_own_wire_name_and_no_detail() {
        let e = BrokerError::CredentialUnreadable("bad_mode");
        assert_eq!(e.code(), "credential_unreadable");
    }
}
