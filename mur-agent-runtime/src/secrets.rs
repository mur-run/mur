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
use std::sync::{Arc, Mutex};

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

    /// Longest stored value in bytes; 0 when the vault is empty. The streaming
    /// masker holds back one less than this between reads.
    pub fn longest_value_len(&self) -> usize {
        self.lock()
            .values()
            .map(|v| v.expose_secret().len())
            .max()
            .unwrap_or(0)
    }

    /// A masker for a byte stream whose chunk boundaries are arbitrary.
    pub fn masker(self: &Arc<Self>) -> StreamMasker {
        StreamMasker {
            vault: Arc::clone(self),
            carry: Vec::new(),
        }
    }

    /// Move `split` left until no stored value straddles it. Emitting
    /// `buf[..split]` is then safe: every occurrence in it is complete, so
    /// `mask` sees it whole. Returns an occurrence START when it moves, which
    /// is a char boundary because every value is a `str`.
    fn safe_split(&self, buf: &[u8], mut split: usize) -> usize {
        let guard = self.lock();
        loop {
            let mut moved = false;
            for v in guard.values() {
                let needle = v.expose_secret().as_bytes();
                if needle.is_empty() || needle.len() > buf.len() {
                    continue;
                }
                for s in 0..=buf.len() - needle.len() {
                    let e = s + needle.len();
                    if s < split && split < e && &buf[s..e] == needle {
                        split = s;
                        moved = true;
                        break;
                    }
                }
            }
            if !moved {
                return split;
            }
        }
    }
}

/// Masks a stream read in arbitrary chunks (D10). `push` returns the bytes
/// that are safe to emit; `finish` flushes what was held back. Between calls
/// it retains `longest_value_len() - 1` bytes (enough that a value cannot end
/// in an already-emitted chunk without having been seen whole) and never
/// splits inside a UTF-8 sequence or inside an occurrence of a value.
pub struct StreamMasker {
    vault: Arc<SecretVault>,
    carry: Vec<u8>,
}

impl StreamMasker {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.carry.extend_from_slice(chunk);
        // Re-read every push: `secret/set` can add a longer value mid-job.
        let hold = self.vault.longest_value_len().saturating_sub(1);
        if self.carry.len() <= hold {
            return Vec::new();
        }
        let mut split = self.carry.len() - hold;
        // Back off a continuation byte so a multi-byte char stays whole.
        // Bounded at 3: past that the bytes are not UTF-8 and lossy is fine.
        let floor = split.saturating_sub(3);
        while split > floor && split < self.carry.len() && (self.carry[split] & 0xC0) == 0x80 {
            split -= 1;
        }
        let split = self.vault.safe_split(&self.carry, split);
        if split == 0 {
            return Vec::new();
        }
        let ready: Vec<u8> = self.carry.drain(..split).collect();
        mask_bytes(&self.vault, &ready)
    }

    pub fn finish(&mut self) -> Vec<u8> {
        let rest = std::mem::take(&mut self.carry);
        mask_bytes(&self.vault, &rest)
    }
}

fn mask_bytes(vault: &SecretVault, bytes: &[u8]) -> Vec<u8> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(bytes);
    vault.mask(&text).into_owned().into_bytes()
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

    fn masked_through(v: &Arc<SecretVault>, a: &[u8], b: &[u8]) -> Vec<u8> {
        let mut m = v.masker();
        let mut out = m.push(a);
        out.extend(m.push(b));
        out.extend(m.finish());
        out
    }

    /// D10, test 9: a secret split at EVERY byte boundary between two pipe
    /// reads is still replaced exactly once, and the plaintext never appears.
    #[test]
    fn stream_masker_catches_a_secret_across_every_split_point() {
        let v = Arc::new(SecretVault::new());
        v.set("PW", "hunter2hunter2").unwrap();
        let text = b"pre hunter2hunter2 post";
        for k in 0..=text.len() {
            let out = masked_through(&v, &text[..k], &text[k..]);
            let s = String::from_utf8(out).unwrap();
            assert!(!s.contains("hunter2hunter2"), "split at {k}: {s}");
            assert_eq!(s.matches("[SECRET:PW]").count(), 1, "split at {k}: {s}");
            assert_eq!(s, "pre [SECRET:PW] post", "split at {k}");
        }
    }

    /// A multi-byte character right at the split must not be cut in half.
    #[test]
    fn stream_masker_keeps_utf8_intact_around_the_split() {
        let v = Arc::new(SecretVault::new());
        v.set("PW", "hunter2hunter2").unwrap();
        let text = "préfix ü hunter2hunter2 ü".as_bytes();
        for k in 0..=text.len() {
            let out = masked_through(&v, &text[..k], &text[k..]);
            let s = String::from_utf8(out).unwrap_or_else(|e| panic!("split at {k}: {e}"));
            assert_eq!(s, "préfix ü [SECRET:PW] ü", "split at {k}");
        }
    }

    /// Two secrets where one is a prefix of the other: the longer one wins,
    /// whatever the split.
    #[test]
    fn stream_masker_prefers_the_longer_secret_across_splits() {
        let v = Arc::new(SecretVault::new());
        v.set("SHORT", "hunter2hunter2").unwrap();
        v.set("LONG", "hunter2hunter2extra").unwrap();
        let text = b"x hunter2hunter2extra y";
        for k in 0..=text.len() {
            let out = masked_through(&v, &text[..k], &text[k..]);
            let s = String::from_utf8(out).unwrap();
            assert_eq!(s, "x [SECRET:LONG] y", "split at {k}: {s}");
        }
    }

    /// An empty vault is a passthrough with no hold-back.
    #[test]
    fn stream_masker_without_secrets_passes_bytes_straight_through() {
        let v = Arc::new(SecretVault::new());
        let mut m = v.masker();
        assert_eq!(m.push(b"abc"), b"abc".to_vec());
        assert_eq!(m.finish(), Vec::<u8>::new());
    }
}
