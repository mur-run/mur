//! The credential record an https endpoint may carry (https design §6): where the secret lives
//! and which username goes with it. Neither field is a secret, so `Debug` stays derived: a path
//! and a name in a log line are what an operator needs to find the problem, and the token itself
//! is never a field here (it is read by `CredentialSource::open` and never stored).
//!
//! Parsing is shape-only and touches no filesystem. Whether the path is under `secrets/`, a
//! regular file, `0600` and owned by the daemon is checked where the file is opened.

use crate::constants::*;
use crate::error::BrokerError;
use serde::{Deserialize, Serialize};

fn bad(m: &str) -> BrokerError {
    BrokerError::InvalidRequest(m.into())
}

/// Absolute path of a secret file. Invalid values cannot be constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretPath(String);

impl SecretPath {
    /// Shape only: absolute; `/`-separated segments, none empty, `.` or `..`; no `$` or `~`
    /// (nothing here is ever expanded, so a value that looks like it should be is a mistake);
    /// no control characters. Spaces and non-ASCII stay legal: a home directory may have them,
    /// and the path is opened directly, never put in a URL.
    pub fn parse(s: &str) -> Result<Self, BrokerError> {
        let body = s.strip_prefix('/').ok_or_else(|| bad("credential_path"))?;
        let seg_ok = |seg: &str| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && !seg
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '$' | '~'))
        };
        if body.split('/').all(seg_ok) {
            Ok(Self(s.to_string()))
        } else {
            Err(bad("credential_path"))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The non-secret half of an https login, as it appears before the `@` in the URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Username(String);

impl Username {
    /// `[A-Za-z0-9._-]`, 1..=`CREDENTIAL_USERNAME_MAX_LEN` bytes: nothing that needs
    /// percent-encoding, so no encoder sits on the path that builds `https://<user>@host/...`.
    pub fn parse(s: &str) -> Result<Self, BrokerError> {
        let ok = (1..=CREDENTIAL_USERNAME_MAX_LEN).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if ok {
            Ok(Self(s.to_string()))
        } else {
            Err(bad("credential_username"))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Serializes as the plain string; deserializes through `parse`, so a hand-edited bad value fails
/// the file load rather than reaching a push.
macro_rules! string_newtype_serde {
    ($t:ident) => {
        impl Serialize for $t {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.0)
            }
        }
        impl<'de> Deserialize<'de> for $t {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                $t::parse(&s).map_err(serde::de::Error::custom)
            }
        }
    };
}
string_newtype_serde!(SecretPath);
string_newtype_serde!(Username);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    pub secret_path: SecretPath,
    pub username: Username,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_path_accepts_an_absolute_clean_path() {
        for s in [
            "/home/u/.mur/secrets/git-token",
            "/secrets/t",
            "/a/b.c/d_e-f",
        ] {
            assert_eq!(SecretPath::parse(s).unwrap().as_str(), s, "{s}");
        }
    }

    #[test]
    fn secret_path_refuses_everything_else() {
        for s in [
            "",
            "/",
            "relative/path",
            "./x",
            "~/secrets/t",
            "/home/~u/secrets/t",
            "$HOME/secrets/t",
            "/home/$USER/secrets/t",
            "/a/../b",
            "/a/./b",
            "/..",
            "/a//b",
            "/a/b/",
            "/a/b\0c",
            "/a/b\nc",
            "/a/b\tc",
            "/a/\u{7f}",
        ] {
            assert!(SecretPath::parse(s).is_err(), "{s:?} must be refused");
        }
    }

    #[test]
    fn username_accepts_the_allowed_alphabet_up_to_the_cap() {
        for s in ["x-access-token", "oauth2", "svc.acct_1", "A", "-", "."] {
            assert_eq!(Username::parse(s).unwrap().as_str(), s, "{s}");
        }
        let max = "a".repeat(CREDENTIAL_USERNAME_MAX_LEN);
        assert!(Username::parse(&max).is_ok());
    }

    #[test]
    fn username_refuses_what_would_need_url_encoding() {
        let too_long = "a".repeat(CREDENTIAL_USERNAME_MAX_LEN + 1);
        for s in [
            "", "a b", "a@b", "a:b", "a/b", "a%40b", "a?b", "a#b", "a\\b", "é", "a\0", "a\n",
            "a+b", "a~b", &too_long,
        ] {
            assert!(Username::parse(s).is_err(), "{s:?} must be refused");
        }
    }

    #[test]
    fn credential_round_trips_through_yaml() {
        let c = Credential {
            secret_path: SecretPath::parse("/h/.mur/secrets/t").unwrap(),
            username: Username::parse("x-access-token").unwrap(),
        };
        let y = serde_yaml_ng::to_string(&c).unwrap();
        assert!(y.contains("secret_path: /h/.mur/secrets/t"), "{y}");
        assert!(y.contains("username: x-access-token"), "{y}");
        assert_eq!(serde_yaml_ng::from_str::<Credential>(&y).unwrap(), c);
    }

    #[test]
    fn a_malformed_field_fails_the_load() {
        for y in [
            "secret_path: rel/t\nusername: u\n",
            "secret_path: /a/../t\nusername: u\n",
            "secret_path: /a/t\nusername: 'a b'\n",
            "secret_path: /a/t\nusername: ''\n",
        ] {
            assert!(serde_yaml_ng::from_str::<Credential>(y).is_err(), "{y}");
        }
    }

    #[test]
    fn a_missing_or_unknown_field_fails_the_load() {
        for y in [
            "secret_path: /a/t\n",
            "username: u\n",
            "secret_path: /a/t\nusername: u\ntoken: hunter2\n",
        ] {
            assert!(serde_yaml_ng::from_str::<Credential>(y).is_err(), "{y}");
        }
    }

    #[test]
    fn debug_shows_the_path_and_name_and_has_nowhere_to_hold_a_token() {
        let c = Credential {
            secret_path: SecretPath::parse("/h/.mur/secrets/t").unwrap(),
            username: Username::parse("oauth2").unwrap(),
        };
        let d = format!("{c:?}");
        assert!(
            d.contains("/h/.mur/secrets/t") && d.contains("oauth2"),
            "{d}"
        );
    }
}
