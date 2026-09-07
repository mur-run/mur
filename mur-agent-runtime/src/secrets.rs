//! Per-process store for credentials the user handed the agent.
//!
//! Filled pre-seal from the agent's keychain slot and mutated by the
//! `secret/set` / `secret/delete` A2A methods. Three readers: the system
//! prompt (names only), the bash tool (env at spawn), and the tool-result
//! path (value masking). The value never appears anywhere else — not in
//! tracing, not in A2A responses, not in telemetry.
//!
//! Masking is defense in depth, not the guarantee: a value the model asked a
//! tool to re-encode (`base64`, `xxd`) is not caught, and the tests below say
//! so on purpose. The guarantee is that the plaintext never enters the
//! context in the first place.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Mutex;

use secrecy::{ExposeSecret, SecretString};

/// Shortest value the vault accepts. Masking is a whole-string replace, so a
/// short value would shred ordinary output (`1234` would eat every number);
/// and nothing under eight characters deserves the name "token". Same floor
/// GitHub Actions warns at.
pub const MIN_LEN: usize = 8;

/// Keychain service under which per-agent secrets live. Must stay in sync
/// with `mur-core/src/cmd/agent/secret.rs::SECRET_SERVICE`.
pub const KEYCHAIN_SERVICE: &str = "mur-agent";

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SecretVaultError {
    #[error("secret name '{0}' must match [A-Z_][A-Z0-9_]*")]
    BadName(String),
    #[error("secret '{0}' is shorter than {MIN_LEN} characters")]
    TooShort(String),
}

/// `[A-Z_][A-Z0-9_]*` — an environment-variable name, because that is what
/// the bash tool will export it as.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

#[derive(Default)]
pub struct SecretVault {
    inner: Mutex<BTreeMap<String, SecretString>>,
}

impl SecretVault {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, name: &str, value: &str) -> Result<(), SecretVaultError> {
        if !valid_name(name) {
            return Err(SecretVaultError::BadName(name.to_string()));
        }
        if value.chars().count() < MIN_LEN {
            return Err(SecretVaultError::TooShort(name.to_string()));
        }
        self.lock()
            .insert(name.to_string(), SecretString::from(value.to_string()));
        Ok(())
    }

    /// `true` if the name was present.
    pub fn remove(&self, name: &str) -> bool {
        self.lock().remove(name).is_some()
    }

    pub fn names(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }

    /// The only place values leave the vault as plain `String`s. For
    /// `Command::envs` at spawn — nowhere else.
    pub fn env_pairs(&self) -> Vec<(String, String)> {
        self.lock()
            .iter()
            .map(|(k, v)| (k.clone(), v.expose_secret().to_string()))
            .collect()
    }

    /// Replace every stored value in `text` with `[SECRET:<NAME>]`. Longest
    /// value first so a value that is a prefix of another cannot split it.
    pub fn mask<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let guard = self.lock();
        let mut ordered: Vec<(&String, &SecretString)> = guard.iter().collect();
        ordered.sort_by_key(|(_, v)| std::cmp::Reverse(v.expose_secret().len()));
        let mut out = Cow::Borrowed(text);
        for (name, value) in ordered {
            let raw = value.expose_secret();
            if out.contains(raw) {
                out = Cow::Owned(out.replace(raw, &format!("[SECRET:{name}]")));
            }
        }
        out
    }

    /// The system-prompt section. Names only. `None` when there is nothing to
    /// say, so an agent with no secrets pays no tokens for this.
    pub fn prompt_fragment(&self) -> Option<String> {
        let names = self.names();
        if names.is_empty() {
            return None;
        }
        let list: Vec<String> = names.iter().map(|n| format!("${n}")).collect();
        Some(format!(
            "\n\n## Secrets available to the bash tool\n\
             These environment variables are set for every bash command you run: {}.\n\
             Use them by name (e.g. `curl -H \"Authorization: token $NAME\"`). \
             Never print their values, never put one into a URL or a remote (it would \
             land in `.git/config`), and prefer a git credential helper over embedding \
             a token in a clone URL.",
            list.join(", ")
        ))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, SecretString>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_env_var_shaped() {
        assert!(valid_name("GITEA_TOKEN"));
        assert!(valid_name("_X"));
        assert!(valid_name("A1"));
        assert!(!valid_name(""));
        assert!(!valid_name("gitea_token"));
        assert!(!valid_name("1ABC"));
        assert!(!valid_name("A-B"));
        assert!(!valid_name("A B"));
    }

    #[test]
    fn set_rejects_a_bad_name_and_a_short_value() {
        let v = SecretVault::new();
        assert_eq!(
            v.set("bad name", "longenough12"),
            Err(SecretVaultError::BadName("bad name".into()))
        );
        assert_eq!(
            v.set("SHORT", "1234567"),
            Err(SecretVaultError::TooShort("SHORT".into()))
        );
        assert!(v.names().is_empty());
    }

    #[test]
    fn set_stores_and_names_are_sorted() {
        let v = SecretVault::new();
        v.set("ZED_TOKEN", "zzzzzzzzzz").unwrap();
        v.set("ALPHA_KEY", "aaaaaaaaaa").unwrap();
        assert_eq!(
            v.names(),
            vec!["ALPHA_KEY".to_string(), "ZED_TOKEN".to_string()]
        );
    }

    #[test]
    fn set_overwrites_and_remove_reports_presence() {
        let v = SecretVault::new();
        v.set("K", "first-value").unwrap();
        v.set("K", "second-value").unwrap();
        assert_eq!(
            v.env_pairs(),
            vec![("K".to_string(), "second-value".to_string())]
        );
        assert!(v.remove("K"));
        assert!(!v.remove("K"));
        assert!(v.env_pairs().is_empty());
    }

    #[test]
    fn mask_replaces_every_value_with_its_name_tag() {
        let v = SecretVault::new();
        v.set("GITEA_TOKEN", "d8b04a3cc632a5c8026cf5a810d36e292c603f99")
            .unwrap();
        v.set("OTHER", "hunter2hunter2").unwrap();
        let out = v.mask(
            "token=d8b04a3cc632a5c8026cf5a810d36e292c603f99 pw=hunter2hunter2 again d8b04a3cc632a5c8026cf5a810d36e292c603f99",
        );
        assert_eq!(
            out,
            "token=[SECRET:GITEA_TOKEN] pw=[SECRET:OTHER] again [SECRET:GITEA_TOKEN]"
        );
    }

    #[test]
    fn mask_borrows_when_nothing_matches() {
        let v = SecretVault::new();
        v.set("K", "not-in-the-text").unwrap();
        let s = "plain output";
        assert!(matches!(v.mask(s), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn mask_handles_one_value_being_a_prefix_of_another() {
        // Longest value first, otherwise the short one would split the long
        // one and leave a fragment of it visible.
        let v = SecretVault::new();
        v.set("SHORT", "abcdefgh").unwrap();
        v.set("LONG", "abcdefghijkl").unwrap();
        assert_eq!(v.mask("x abcdefghijkl y"), "x [SECRET:LONG] y");
    }

    /// The documented ceiling: masking is a string replace, so a value the
    /// tool re-encoded is not caught. This test exists so nobody "fixes" the
    /// docs to claim otherwise without also changing the mechanism.
    #[test]
    fn mask_does_not_catch_a_reencoded_value() {
        let v = SecretVault::new();
        v.set("K", "hunter2hunter2").unwrap();
        let b64 = "aHVudGVyMmh1bnRlcjI="; // base64("hunter2hunter2")
        assert_eq!(v.mask(b64), b64);
    }

    #[test]
    fn prompt_fragment_lists_names_and_never_values() {
        let v = SecretVault::new();
        assert_eq!(v.prompt_fragment(), None);
        v.set("GITEA_TOKEN", "d8b04a3cc632a5c8026cf5a810d36e292c603f99")
            .unwrap();
        let f = v.prompt_fragment().unwrap();
        assert!(f.contains("$GITEA_TOKEN"), "{f}");
        assert!(!f.contains("d8b04a3c"), "{f}");
        assert!(
            f.contains("Authorization"),
            "the git guidance line is part of the fragment: {f}"
        );
    }
}
