//! The `enroll` login flags and how they become the credential stored on the record.

use mur_git_broker::constants::ENDPOINT_HTTPS_SCHEME;
use mur_git_broker::credential::{Credential, SecretPath, Username};
use mur_git_broker::enrollment::Endpoint;

/// How an `https://` endpoint logs in. A `file://` endpoint takes none of these.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct EnrollAuth {
    /// Username for an `https://` endpoint (the part before `@`). Needs `--secret`.
    #[arg(long, requires = "secret")]
    pub username: Option<String>,
    /// Absolute path of the token file for an `https://` endpoint: a 0600 file you own, under
    /// `~/.mur/secrets/`. Needs `--username`.
    #[arg(long, requires = "username")]
    pub secret: Option<String>,
    /// Enroll an `https://` endpoint with no credential, for a remote that accepts anonymous
    /// pushes. Without this, an `https://` endpoint with no `--username`/`--secret` is refused.
    #[arg(long, conflicts_with_all = ["username", "secret"])]
    pub allow_unauthenticated: bool,
}

/// The credential to store for `endpoint`, or why the flags do not fit it. `Ok(None)` means
/// "enroll with no credential", which only a `file://` endpoint or an explicit
/// `--allow-unauthenticated` gets: an https push with no credential is refused by most hosts,
/// and finding that out after a human has spent an approval on it (approvals are never retried)
/// is the expensive way.
pub(super) fn credential_for(
    endpoint: &Endpoint,
    auth: &EnrollAuth,
) -> Result<Option<Credential>, String> {
    let https = endpoint.as_str().starts_with(ENDPOINT_HTTPS_SCHEME);
    let given = auth.username.is_some() || auth.secret.is_some() || auth.allow_unauthenticated;
    if !https {
        return if given {
            Err(
                "--username, --secret and --allow-unauthenticated only apply to an https:// \
                 endpoint"
                    .into(),
            )
        } else {
            Ok(None)
        };
    }
    match (&auth.username, &auth.secret) {
        (Some(user), Some(secret)) => {
            let username = Username::parse(user).map_err(|_| {
                format!("--username {user:?} must be 1+ characters of [A-Za-z0-9._-]")
            })?;
            let secret_path = SecretPath::parse(secret).map_err(|_| {
                format!(
                    "--secret {secret:?} must be an absolute path with no `~`, `$`, `.` or `..` \
                     segments (nothing is expanded)"
                )
            })?;
            Ok(Some(Credential {
                secret_path,
                username,
            }))
        }
        (None, None) if auth.allow_unauthenticated => Ok(None),
        (None, None) => Err(format!(
            "endpoint {} is https:// but has no credential; give --username and --secret, or \
             --allow-unauthenticated if the remote accepts anonymous pushes",
            endpoint.as_str()
        )),
        // clap's `requires` keeps these off the command line; a direct caller still gets told.
        _ => Err("--username and --secret go together".into()),
    }
}
