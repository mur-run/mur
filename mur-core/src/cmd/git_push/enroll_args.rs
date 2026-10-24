//! The `enroll` login flags and how they become the credentials stored on the record.

use mur_git_broker::constants::ENDPOINT_HTTPS_SCHEME;
use mur_git_broker::credential::{Credential, SecretPath, Username};
use mur_git_broker::enrollment::Endpoint;

/// How an `https://` endpoint logs in. A `file://` endpoint takes none of these.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct EnrollAuth {
    /// Write (push) credential: the username for an `https://` endpoint (the part before `@`).
    /// Needs `--secret`.
    #[arg(long, requires = "secret")]
    pub username: Option<String>,
    /// Write (push) credential: absolute path of the token file for an `https://` endpoint, a
    /// 0600 file you own under `~/.mur/secrets/`. The file must already exist; enroll never
    /// writes it. Needs `--username`.
    #[arg(long, requires = "username")]
    pub secret: Option<String>,
    /// Read credential: the username for the read-only fetch that runs before a human approves.
    /// Needs `--read-secret` and `--username`. Optional; without it the write credential is used
    /// for both.
    #[arg(long, requires_all = ["read_secret", "username"], conflicts_with = "allow_unauthenticated")]
    pub read_username: Option<String>,
    /// Read credential: absolute path of an existing 0600 token file under `~/.mur/secrets/`,
    /// a different token from `--secret`. With it the push token is not opened until a human
    /// approves. mur only selects which file is opened; it cannot check what the host lets the
    /// read token do, so making it read-only is up to you. Needs `--read-username`.
    #[arg(long, requires_all = ["read_username", "username"], conflicts_with = "allow_unauthenticated")]
    pub read_secret: Option<String>,
    /// Enroll an `https://` endpoint with no credential, for a remote that accepts anonymous
    /// pushes. Without this, an `https://` endpoint with no `--username`/`--secret` is refused.
    #[arg(long, conflicts_with_all = ["username", "secret"])]
    pub allow_unauthenticated: bool,
}

/// The logins to store for an endpoint: the write credential and the optional read credential.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Logins {
    pub credential: Option<Credential>,
    pub read_credential: Option<Credential>,
}

/// One `--<user_flag>` / `--<secret_flag>` pair as a credential, or why it is malformed.
fn login(
    (user_flag, secret_flag): (&str, &str),
    user: &str,
    secret: &str,
) -> Result<Credential, String> {
    let username = Username::parse(user)
        .map_err(|_| format!("{user_flag} {user:?} must be 1+ characters of [A-Za-z0-9._-]"))?;
    let secret_path = SecretPath::parse(secret).map_err(|_| {
        format!(
            "{secret_flag} {secret:?} must be an absolute path with no `~`, `$`, `.` or `..` \
             segments (nothing is expanded)"
        )
    })?;
    Ok(Credential {
        secret_path,
        username,
    })
}

/// The logins to store for `endpoint`, or why the flags do not fit it. No credential at all
/// (`Logins::default()`) is what only a `file://` endpoint or an explicit
/// `--allow-unauthenticated` gets: an https push with no credential is refused by most hosts,
/// and finding that out after a human has spent an approval on it (approvals are never retried)
/// is the expensive way.
///
/// Only the shape of the flags is checked here. Whether a secret file is usable, and whether the
/// read and write secrets are the same token, is the doctor's `credential_problems`, which
/// `enroll` runs on the result.
pub(super) fn credentials_for(endpoint: &Endpoint, auth: &EnrollAuth) -> Result<Logins, String> {
    let https = endpoint.as_str().starts_with(ENDPOINT_HTTPS_SCHEME);
    let read_given = auth.read_username.is_some() || auth.read_secret.is_some();
    let given = auth.username.is_some()
        || auth.secret.is_some()
        || read_given
        || auth.allow_unauthenticated;
    if !https {
        return if given {
            Err(
                "--username, --secret, --read-username, --read-secret and --allow-unauthenticated \
                 only apply to an https:// endpoint"
                    .into(),
            )
        } else {
            Ok(Logins::default())
        };
    }
    // clap keeps these combinations off the command line; a direct caller still gets told.
    if read_given && auth.allow_unauthenticated {
        return Err(
            "--read-username and --read-secret cannot be combined with --allow-unauthenticated \
             (an unauthenticated enrollment has no credential to split)"
                .into(),
        );
    }
    let read = match (&auth.read_username, &auth.read_secret) {
        (Some(_), None) => return Err("--read-username needs --read-secret".into()),
        (None, Some(_)) => return Err("--read-secret needs --read-username".into()),
        (Some(user), Some(secret)) => {
            if auth.username.is_none() || auth.secret.is_none() {
                return Err(
                    "--read-username and --read-secret need --username and --secret: a read \
                     credential only splits an enrollment that has a write credential"
                        .into(),
                );
            }
            Some((user, secret))
        }
        (None, None) => None,
    };
    match (&auth.username, &auth.secret) {
        (Some(user), Some(secret)) => Ok(Logins {
            credential: Some(login(("--username", "--secret"), user, secret)?),
            read_credential: read
                .map(|(u, s)| login(("--read-username", "--read-secret"), u, s))
                .transpose()?,
        }),
        (None, None) if auth.allow_unauthenticated => Ok(Logins::default()),
        (None, None) => Err(format!(
            "endpoint {} is https:// but has no credential; give --username and --secret, or \
             --allow-unauthenticated if the remote accepts anonymous pushes",
            endpoint.as_str()
        )),
        _ => Err("--username and --secret go together".into()),
    }
}

/// What one enrollment was enrolled with, enough to print the `enroll` command that recreates it.
pub(super) struct Reenroll<'a> {
    pub agent: &'a str,
    pub repo: &'a str,
    pub remote: &'a str,
    pub endpoint: &'a str,
    pub ref_prefix: &'a str,
    pub base_refs: &'a [String],
    pub credential: Option<&'a Credential>,
}

/// `s` as one shell word: bare when it is made of safe characters, otherwise single-quoted.
fn shell_word(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:@%+=,".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

impl Reenroll<'_> {
    /// The copy-pasteable command that re-enrolls this record with a read login added. Every value
    /// comes from the record; only the read login, which does not exist yet, is a placeholder.
    /// The record's ref prefix and base refs are repeated so re-running it changes nothing else.
    pub fn command_with_read_login(&self) -> String {
        let mut c = format!(
            "mur git-push enroll --agent {} --repo {} --remote {} --endpoint {} --ref-prefix {}",
            shell_word(self.agent),
            shell_word(self.repo),
            shell_word(self.remote),
            shell_word(self.endpoint),
            shell_word(self.ref_prefix),
        );
        for r in self.base_refs {
            c.push_str(&format!(" --base-ref {}", shell_word(r)));
        }
        let (user, secret) = match self.credential {
            Some(w) => (
                shell_word(w.username.as_str()),
                shell_word(w.secret_path.as_str()),
            ),
            None => ("<USER>".into(), "<SECRET_PATH>".into()),
        };
        c.push_str(&format!(" --username {user} --secret {secret}"));
        c.push_str(" --read-username <READ_USER> --read-secret <READ_SECRET_PATH>");
        c
    }
}
