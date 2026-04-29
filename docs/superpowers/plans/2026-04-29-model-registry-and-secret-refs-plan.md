# Model Registry and Secret References Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Introduce a named model registry (`~/.mur/models.yaml`) plus a typed `SecretRef` abstraction (env / keychain / file / cmd, with .age decryption) so multiple agents can share provider+model definitions, share or differentiate API keys cleanly, and have GUI-launched sidecars pick up credentials without inheriting shell env.

**Architecture:** Add two new modules to `mur-common` (`secret`, `model`), plumb resolution into `mur-agent-runtime`, add CLI verbs `mur model …` and `mur agent secret …`, then make the Tauri main resolve secrets and inject them via `Command::env()` into the sidecar. Frontend gets a Model tab for switching active model + setting secrets without leaving the GUI. Legacy inline `model:` block in `profile.yaml` keeps working.

**Tech Stack:** Rust 2024, Tokio, `serde` + `serde_yaml_ng`, `keyring` v4 (OS keychain), `age` (file encryption), `secrecy` (in-memory zeroize), `thiserror`, `tracing`, Tauri 2 + React 18 (frontend).

**Working directory:** `/Users/david/Projects/mur/.worktrees/model-registry/` on branch `feat/model-registry-and-secret-refs`. All workspace tests are green at baseline (1200295).

**Spec:** `docs/superpowers/specs/2026-04-29-model-registry-and-secret-refs-design.md`

---

## Conventions used by every task

- All file paths are relative to the worktree root unless absolute.
- Every task ends with **Run** + **Expected** + **Commit**. The expected output is what tells us the task is done — do not move on until you see it.
- Commit messages follow the project's existing style (`feat(scope): …`, `fix(scope): …`, `test(scope): …`, `docs(scope): …`).
- Co-author trailer is mandatory: `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`.
- TDD: when adding behavior, write the failing test first; commit the failing test in its own commit if the implementation is non-trivial; otherwise bundle.
- Keep clippy + fmt clean (`cargo clippy --workspace -- -D warnings && cargo fmt --check`) before every commit on a Rust file.

## Reference points already in the codebase

- `mur-common/src/agent.rs:130-135` — current `ModelConfig { provider, name, params }`. Stays as legacy fallback.
- `mur-common/src/lib.rs` — module list. Add `pub mod secret;` and `pub mod model;`.
- `mur-agent-runtime/src/supervisor.rs:145-202` — current provider-dispatch site. Wire registry resolution here.
- `mur-agent-runtime/src/llm/anthropic.rs:55-64` — `AnthropicClient::from_env`. Add a sibling `from_secret_ref(SecretRef)`.
- `mur-agent-runtime/src/llm/openai.rs` + `ollama.rs` — same pattern.
- `mur-core/src/cmd/agent.rs` — clap subcommands for `mur agent …`. Add `secret` subcommand here.
- `mur-core/src/cmd/mod.rs` — top-level command registration. Add `pub mod model;`.
- `mur-agent-gui/src-tauri/src/sidecar.rs` — sidecar `Command` builder. Inject env here.
- `mur-agent-gui/src-tauri/src/commands.rs` + `main.rs::invoke_handler` — Tauri commands.
- `mur-agent-gui/ui/src/App.tsx::TABS` + `mur-agent-gui/ui/src/tabs/` — frontend tabs.

---

# PR-1 — `mur-common::secret` module

**Why first:** Everything else depends on `SecretRef`. Ship it standalone with full unit-test coverage, no consumers yet.

## Task 1.1 — Add dependencies to `mur-common/Cargo.toml`

**File:** `mur-common/Cargo.toml`

**Step 1: Edit the `[dependencies]` block.** Append:

```toml
keyring = { version = "4", default-features = false, features = ["apple-native", "windows-native", "sync-secret-service", "linux-native"] }
secrecy = "0.10"
age = { version = "0.11", features = ["armor"] }
tokio = { workspace = true, features = ["fs", "process", "rt"] }
shellexpand = "3"
```

(Note: workspace `tokio` already exists. We need the `fs` / `process` / `rt` features for the resolver. If the workspace declaration is leaner, add it only at the crate level.)

**Step 2: Build.**

Run: `cargo build -p mur-common 2>&1 | tail -5`
Expected: `Finished \`dev\` profile`. If `keyring` features mismatch, fall back to `default-features = true` and re-build to confirm.

**Step 3: Commit.**

```bash
git add mur-common/Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
build(common): add keyring/secrecy/age/shellexpand for SecretRef

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Task 1.2 — `SecretRef` enum skeleton + serde codec

**Files:**
- Create: `mur-common/src/secret.rs`
- Modify: `mur-common/src/lib.rs` (add `pub mod secret;`)

**Step 1: Write failing tests first.** Create `mur-common/src/secret.rs` with:

```rust
//! Typed reference to a secret value. The reference itself is safe to
//! commit / log / serialize; the resolved value (`SecretString`) is
//! zeroized on drop.
//!
//! Wire format is a single string with a colon-prefixed scheme:
//!   env:VAR_NAME
//!   keychain:service/account
//!   file:/absolute/or/~-path[.age]
//!   cmd:./script-or-binary args…

use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretRef {
    Env(String),
    Keychain { service: String, account: String },
    File(PathBuf),
    Cmd(String),
}

#[derive(thiserror::Error, Debug)]
pub enum SecretError {
    #[error("env var {0} not set")]
    EnvNotSet(String),
    #[error("keychain item not found: {service}/{account}")]
    KeychainNotFound { service: String, account: String },
    #[error("keychain backend error: {0}")]
    KeychainBackend(String),
    #[error("read file {path}: {source}")]
    FileRead { path: String, #[source] source: std::io::Error },
    #[error("file mode is not 0600: {0}")]
    FileMode(String),
    #[error("decrypt {0}")]
    AgeDecrypt(String),
    #[error("cmd {cmd} exited with {status}")]
    Cmd { cmd: String, status: i32 },
    #[error("invalid SecretRef syntax: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::Error as _;
    use serde_yaml_ng as yaml;

    #[test]
    fn parses_env_form() {
        let s: SecretRef = yaml::from_str("env:ANTHROPIC_API_KEY").unwrap();
        assert_eq!(s, SecretRef::Env("ANTHROPIC_API_KEY".into()));
    }

    #[test]
    fn parses_keychain_form() {
        let s: SecretRef = yaml::from_str("keychain:mur/anthropic-oauth").unwrap();
        assert_eq!(s, SecretRef::Keychain { service: "mur".into(), account: "anthropic-oauth".into() });
    }

    #[test]
    fn parses_file_form() {
        let s: SecretRef = yaml::from_str("file:/tmp/foo.age").unwrap();
        assert_eq!(s, SecretRef::File(PathBuf::from("/tmp/foo.age")));
    }

    #[test]
    fn parses_cmd_form() {
        let s: SecretRef = yaml::from_str("cmd:op read op://vault/item/field").unwrap();
        assert_eq!(s, SecretRef::Cmd("op read op://vault/item/field".into()));
    }

    #[test]
    fn rejects_unknown_scheme() {
        let r: Result<SecretRef, _> = yaml::from_str("plain:supersecret");
        assert!(r.is_err());
    }

    #[test]
    fn round_trip_serde() {
        let cases = [
            "env:X",
            "keychain:svc/acct",
            "file:/p",
            "cmd:bin --flag arg",
        ];
        for s in cases {
            let parsed: SecretRef = yaml::from_str(s).unwrap();
            let back = yaml::to_string(&parsed).unwrap();
            // serde-yaml adds a trailing newline / quoting. Strip and compare.
            let normalized = back.trim().trim_matches(|c: char| c == '"' || c == '\'').to_string();
            let reparsed: SecretRef = yaml::from_str(&normalized).unwrap();
            assert_eq!(parsed, reparsed, "round-trip drift for {s}");
        }
    }
}
```

Don't add the impls yet — we want the test file to compile-fail first to confirm we're testing what we think.

**Step 2: Implement `Serialize` + `Deserialize` via custom string codec.** Append to `secret.rs`:

```rust
impl std::fmt::Display for SecretRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretRef::Env(v) => write!(f, "env:{v}"),
            SecretRef::Keychain { service, account } => {
                write!(f, "keychain:{service}/{account}")
            }
            SecretRef::File(p) => write!(f, "file:{}", p.display()),
            SecretRef::Cmd(c) => write!(f, "cmd:{c}"),
        }
    }
}

impl std::str::FromStr for SecretRef {
    type Err = SecretError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (scheme, rest) = s
            .split_once(':')
            .ok_or_else(|| SecretError::Parse(format!("missing scheme: {s}")))?;
        match scheme {
            "env" => Ok(SecretRef::Env(rest.to_string())),
            "keychain" => {
                let (service, account) = rest.split_once('/').ok_or_else(|| {
                    SecretError::Parse(format!("keychain ref needs service/account: {s}"))
                })?;
                Ok(SecretRef::Keychain {
                    service: service.to_string(),
                    account: account.to_string(),
                })
            }
            "file" => Ok(SecretRef::File(PathBuf::from(rest))),
            "cmd" => Ok(SecretRef::Cmd(rest.to_string())),
            other => Err(SecretError::Parse(format!("unknown scheme: {other}"))),
        }
    }
}

impl Serialize for SecretRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}
```

**Step 3: Register module.** In `mur-common/src/lib.rs`, add `pub mod secret;` near the other `pub mod` lines (alphabetical).

**Step 4: Run tests.**

Run: `cargo test -p mur-common secret::tests 2>&1 | tail -20`
Expected: `test result: ok. 6 passed; 0 failed`.

**Step 5: Commit.**

```bash
git add mur-common/src/secret.rs mur-common/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(common): SecretRef enum with serde string codec

Variants: Env, Keychain {service, account}, File(path), Cmd(string).
Wire format is a single colon-prefixed string (env:X, keychain:svc/acct,
file:/p, cmd:…) so config files stay terse and human-readable. Mirrors
mur-commander's engine::secret shape exactly so a future merge into a
shared crate is mechanical.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Task 1.3 — `SecretRef::resolve` for `Env` variant

**File:** `mur-common/src/secret.rs`

**Step 1: Test first.** Append:

```rust
#[cfg(test)]
mod resolve_env_tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[tokio::test]
    async fn resolves_env_when_set() {
        // SAFETY: test is single-threaded by virtue of unique env var name.
        unsafe { std::env::set_var("MUR_TEST_RESOLVE_ENV", "shhh"); }
        let s = SecretRef::Env("MUR_TEST_RESOLVE_ENV".into());
        let v = s.resolve().await.unwrap();
        assert_eq!(v.expose_secret(), "shhh");
    }

    #[tokio::test]
    async fn errors_when_env_missing() {
        let s = SecretRef::Env("MUR_TEST_DEFINITELY_UNSET".into());
        let err = s.resolve().await.unwrap_err();
        assert!(matches!(err, SecretError::EnvNotSet(_)), "got {err:?}");
    }
}
```

**Step 2: Implement just the env path.** Append:

```rust
impl SecretRef {
    pub async fn resolve(&self) -> Result<SecretString, SecretError> {
        match self {
            SecretRef::Env(var) => std::env::var(var)
                .map(SecretString::from)
                .map_err(|_| SecretError::EnvNotSet(var.clone())),
            // Other variants — added in subsequent tasks.
            _ => Err(SecretError::Parse(
                "resolve not implemented for this variant yet".into(),
            )),
        }
    }
}
```

Add `tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }` to `[dev-dependencies]` if not already there.

**Step 3: Run.**

Run: `cargo test -p mur-common secret::resolve_env_tests 2>&1 | tail -10`
Expected: `2 passed`.

**Step 4: Commit.**

```bash
git add mur-common/src/secret.rs mur-common/Cargo.toml
git commit -m "$(cat <<'EOF'
feat(common): SecretRef::resolve for Env variant

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Task 1.4 — Resolve `Keychain` variant via `keyring` crate

**File:** `mur-common/src/secret.rs`

**Step 1: Test first** — but use the keyring `mock` feature so we're not asking the developer to populate the real OS keychain. In `mur-common/Cargo.toml` `[dev-dependencies]`, add `keyring = { version = "4", features = ["sync-secret-service", "linux-native", "apple-native", "windows-native", "mock"], default-features = false }` (or just enable `mock` on the existing keyring entry via dev-deps duplicate). Append to `secret.rs`:

```rust
#[cfg(test)]
mod resolve_keychain_tests {
    use super::*;
    use secrecy::ExposeSecret;

    fn mock_setup(value: Option<&str>) {
        // The keyring `mock` feature replaces the platform backend with an
        // in-memory map keyed on (service, account).
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        let entry = keyring::Entry::new("mur-test", "kc-acct").unwrap();
        let _ = entry.delete_credential();
        if let Some(v) = value { entry.set_password(v).unwrap(); }
    }

    #[tokio::test]
    async fn resolves_when_set() {
        mock_setup(Some("kc-secret"));
        let s = SecretRef::Keychain { service: "mur-test".into(), account: "kc-acct".into() };
        let v = s.resolve().await.unwrap();
        assert_eq!(v.expose_secret(), "kc-secret");
    }

    #[tokio::test]
    async fn errors_when_missing() {
        mock_setup(None);
        let s = SecretRef::Keychain { service: "mur-test".into(), account: "kc-acct".into() };
        let err = s.resolve().await.unwrap_err();
        assert!(matches!(err, SecretError::KeychainNotFound { .. }), "got {err:?}");
    }
}
```

**Step 2: Implement.** In the existing `resolve()` `match`, replace the `_` arm for `Keychain`:

```rust
SecretRef::Keychain { service, account } => {
    let svc = service.clone();
    let acct = account.clone();
    let res = tokio::task::spawn_blocking(move || -> Result<String, SecretError> {
        let entry = keyring::Entry::new(&svc, &acct)
            .map_err(|e| SecretError::KeychainBackend(e.to_string()))?;
        match entry.get_password() {
            Ok(s) => Ok(s),
            Err(keyring::Error::NoEntry) => Err(SecretError::KeychainNotFound {
                service: svc.clone(),
                account: acct.clone(),
            }),
            Err(e) => Err(SecretError::KeychainBackend(e.to_string())),
        }
    })
    .await
    .map_err(|e| SecretError::KeychainBackend(format!("join: {e}")))?;
    res.map(SecretString::from)
}
```

**Step 3: Run.**

Run: `cargo test -p mur-common secret::resolve_keychain_tests 2>&1 | tail -10`
Expected: `2 passed`. (If the mock feature lookup fails, try `cargo tree -p mur-common -e features` to confirm the dev-dep enables `mock`.)

**Step 4: Commit.**

```bash
git add mur-common/src/secret.rs mur-common/Cargo.toml
git commit -m "$(cat <<'EOF'
feat(common): SecretRef::resolve for Keychain variant via keyring v4

Wraps the sync keyring API in spawn_blocking so callers in the Tokio
runtime don't block. Tests use the keyring mock backend.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Task 1.5 — Resolve `File` variant (plaintext + .age)

**File:** `mur-common/src/secret.rs`

**Step 1: Test first.**

```rust
#[cfg(test)]
mod resolve_file_tests {
    use super::*;
    use secrecy::ExposeSecret;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[tokio::test]
    async fn reads_plaintext_0600() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("k.txt");
        std::fs::write(&p, "abc\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        let s = SecretRef::File(p);
        let v = s.resolve().await.unwrap();
        assert_eq!(v.expose_secret(), "abc"); // trailing newline stripped
    }

    #[tokio::test]
    async fn rejects_world_readable() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("k.txt");
        std::fs::write(&p, "abc").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        let s = SecretRef::File(p);
        let err = s.resolve().await.unwrap_err();
        assert!(matches!(err, SecretError::FileMode(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn decrypts_age_passphrase_file() {
        // Generate an age key, encrypt a payload to it, write to disk,
        // then export AGE_IDENTITY env so resolve() can find it.
        let dir = tempdir().unwrap();
        let identity = age::x25519::Identity::generate();
        let recipient = identity.to_public();
        let payload = b"shh-from-age";
        let mut encrypted: Vec<u8> = Vec::new();
        let encryptor = age::Encryptor::with_recipients(vec![Box::new(recipient)]).unwrap();
        let mut writer = encryptor.wrap_output(&mut encrypted).unwrap();
        std::io::Write::write_all(&mut writer, payload).unwrap();
        writer.finish().unwrap();
        let enc_path = dir.path().join("k.age");
        std::fs::write(&enc_path, &encrypted).unwrap();
        std::fs::set_permissions(&enc_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let id_path = dir.path().join("identity.txt");
        std::fs::write(&id_path, identity.to_string().expose_secret()).unwrap();
        std::fs::set_permissions(&id_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        unsafe { std::env::set_var("MUR_AGE_IDENTITY_PATH", &id_path); }
        let s = SecretRef::File(enc_path);
        let v = s.resolve().await.unwrap();
        assert_eq!(v.expose_secret(), "shh-from-age");
        unsafe { std::env::remove_var("MUR_AGE_IDENTITY_PATH"); }
    }
}
```

**Step 2: Implement.** Replace the `_` arm for `File`:

```rust
SecretRef::File(path) => {
    let expanded = shellexpand::full(&path.to_string_lossy())
        .map_err(|e| SecretError::Parse(format!("expand {path:?}: {e}")))?
        .to_string();
    let p = std::path::PathBuf::from(expanded);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = tokio::fs::metadata(&p).await.map_err(|e| SecretError::FileRead {
            path: p.display().to_string(), source: e,
        })?;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(SecretError::FileMode(format!(
                "{}: mode {:o} grants group/world access", p.display(), mode
            )));
        }
    }

    let bytes = tokio::fs::read(&p).await.map_err(|e| SecretError::FileRead {
        path: p.display().to_string(), source: e,
    })?;

    let plaintext = if p.extension().and_then(|s| s.to_str()) == Some("age") {
        decrypt_age(&bytes).await?
    } else {
        String::from_utf8(bytes).map_err(|e| SecretError::AgeDecrypt(e.to_string()))?
    };
    let trimmed = plaintext.trim_end_matches(['\n', '\r']).to_string();
    Ok(SecretString::from(trimmed))
}
```

And add a private helper at the bottom of the file:

```rust
async fn decrypt_age(bytes: &[u8]) -> Result<String, SecretError> {
    let id_path = std::env::var("MUR_AGE_IDENTITY_PATH")
        .or_else(|_| {
            let home = dirs::home_dir().ok_or(())?;
            Ok::<String, ()>(home.join(".mur/age/identity.txt").display().to_string())
        })
        .map_err(|_| SecretError::AgeDecrypt("MUR_AGE_IDENTITY_PATH unset and ~/.mur/age/identity.txt not resolvable".into()))?;

    let id_str = tokio::fs::read_to_string(&id_path).await.map_err(|e|
        SecretError::AgeDecrypt(format!("read identity {id_path}: {e}")))?;
    let identity: age::x25519::Identity = id_str.trim().parse()
        .map_err(|e: &str| SecretError::AgeDecrypt(format!("parse identity: {e}")))?;
    let decryptor = age::Decryptor::new(bytes)
        .map_err(|e| SecretError::AgeDecrypt(e.to_string()))?;
    let mut reader = match decryptor {
        age::Decryptor::Recipients(d) => d
            .decrypt(std::iter::once(&identity as &dyn age::Identity))
            .map_err(|e| SecretError::AgeDecrypt(e.to_string()))?,
        _ => return Err(SecretError::AgeDecrypt("expected recipients-encrypted blob".into())),
    };
    let mut out = String::new();
    use std::io::Read;
    reader.read_to_string(&mut out).map_err(|e| SecretError::AgeDecrypt(e.to_string()))?;
    Ok(out)
}
```

(NOTE: `age` API has changed across versions; if the above doesn't compile against `age = "0.11"`, consult `https://docs.rs/age/0.11.x` for the exact `Decryptor` shape. Do not invent an API.)

**Step 3: Run.**

Run: `cargo test -p mur-common secret::resolve_file_tests 2>&1 | tail -15`
Expected: `3 passed`.

**Step 4: Commit.**

```bash
git add mur-common/src/secret.rs
git commit -m "$(cat <<'EOF'
feat(common): SecretRef::resolve for File variant (plaintext + .age)

Refuses non-0600 mode on Unix. Auto-detects .age suffix and decrypts
using identity at MUR_AGE_IDENTITY_PATH or ~/.mur/age/identity.txt.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Task 1.6 — Resolve `Cmd` variant

**File:** `mur-common/src/secret.rs`

**Step 1: Test first.**

```rust
#[cfg(test)]
mod resolve_cmd_tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[tokio::test]
    async fn echoes_stdout() {
        let s = SecretRef::Cmd("printf shh-from-cmd".into());
        let v = s.resolve().await.unwrap();
        assert_eq!(v.expose_secret(), "shh-from-cmd");
    }

    #[tokio::test]
    async fn errors_on_non_zero_exit() {
        let s = SecretRef::Cmd("sh -c 'exit 7'".into());
        let err = s.resolve().await.unwrap_err();
        match err {
            SecretError::Cmd { status, .. } => assert_eq!(status, 7),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
```

**Step 2: Implement.** Replace the `_` arm for `Cmd`:

```rust
SecretRef::Cmd(spec) => {
    let mut parts = shell_words::split(spec)
        .map_err(|e| SecretError::Parse(format!("split cmd {spec:?}: {e}")))?;
    if parts.is_empty() {
        return Err(SecretError::Parse("empty cmd".into()));
    }
    let program = parts.remove(0);
    let output = tokio::process::Command::new(&program)
        .args(&parts)
        .output()
        .await
        .map_err(|e| SecretError::Cmd { cmd: spec.clone(), status: -1 })
        .or_else(|err| Err(err))?;
    if !output.status.success() {
        return Err(SecretError::Cmd {
            cmd: spec.clone(),
            status: output.status.code().unwrap_or(-1),
        });
    }
    let s = String::from_utf8(output.stdout)
        .map_err(|e| SecretError::Cmd { cmd: spec.clone(), status: -2 })?;
    Ok(SecretString::from(s.trim_end_matches(['\n', '\r']).to_string()))
}
```

Add `shell-words = "1"` to `[dependencies]`.

**Step 3: Run.**

Run: `cargo test -p mur-common secret:: 2>&1 | tail -15`
Expected: 13 passing across all secret modules.

**Step 4: Commit.**

```bash
git add mur-common/src/secret.rs mur-common/Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
feat(common): SecretRef::resolve for Cmd variant

Tokenises the spec via shell-words, runs via tokio::process::Command,
returns trimmed stdout on exit 0 and SecretError::Cmd otherwise.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Task 1.7 — `SecretRef::check()` non-leaking status probe

**File:** `mur-common/src/secret.rs`

**Step 1: Test.**

```rust
#[cfg(test)]
mod check_tests {
    use super::*;

    #[tokio::test]
    async fn check_env_present() {
        unsafe { std::env::set_var("MUR_TEST_CHECK_ENV", "1"); }
        assert!(SecretRef::Env("MUR_TEST_CHECK_ENV".into()).check().await);
    }

    #[tokio::test]
    async fn check_env_absent() {
        assert!(!SecretRef::Env("MUR_TEST_CHECK_DEFINITELY_UNSET".into()).check().await);
    }
}
```

**Step 2: Implement.**

```rust
impl SecretRef {
    /// Check whether the secret can be resolved without leaking the value.
    /// Used by GUI status indicators.
    pub async fn check(&self) -> bool {
        self.resolve().await.is_ok()
    }
}
```

(`Cmd` checking actually invokes the command — if that's expensive in practice, the GUI will note `unknown` on Cmd refs. Acceptable for v1.)

**Step 3: Run.**

Run: `cargo test -p mur-common secret::check_tests 2>&1 | tail -10`
Expected: `2 passed`.

**Step 4: Commit.**

```bash
git add mur-common/src/secret.rs
git commit -m "$(cat <<'EOF'
feat(common): SecretRef::check for non-leaking status probes

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Task 1.8 — Helper `mur-common::secret::keychain` for write-side ops

**Files:** `mur-common/src/secret.rs`

**Step 1: Test.**

```rust
#[cfg(test)]
mod keychain_helpers_tests {
    use super::*;

    #[tokio::test]
    async fn set_then_resolve_round_trips() {
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        keychain_set("mur-test", "round-trip", "v1").await.unwrap();
        let v = SecretRef::Keychain { service: "mur-test".into(), account: "round-trip".into() }
            .resolve().await.unwrap();
        use secrecy::ExposeSecret;
        assert_eq!(v.expose_secret(), "v1");
    }

    #[tokio::test]
    async fn delete_works() {
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        keychain_set("mur-test", "to-delete", "v").await.unwrap();
        keychain_delete("mur-test", "to-delete").await.unwrap();
        let r = SecretRef::Keychain { service: "mur-test".into(), account: "to-delete".into() }
            .resolve().await;
        assert!(matches!(r, Err(SecretError::KeychainNotFound { .. })));
    }
}
```

**Step 2: Implement public helpers.**

```rust
pub async fn keychain_set(service: &str, account: &str, value: &str) -> Result<(), SecretError> {
    let svc = service.to_string();
    let acct = account.to_string();
    let val = value.to_string();
    tokio::task::spawn_blocking(move || -> Result<(), SecretError> {
        let entry = keyring::Entry::new(&svc, &acct)
            .map_err(|e| SecretError::KeychainBackend(e.to_string()))?;
        entry.set_password(&val)
            .map_err(|e| SecretError::KeychainBackend(e.to_string()))?;
        Ok(())
    })
    .await
    .map_err(|e| SecretError::KeychainBackend(format!("join: {e}")))?
}

pub async fn keychain_delete(service: &str, account: &str) -> Result<(), SecretError> {
    let svc = service.to_string();
    let acct = account.to_string();
    tokio::task::spawn_blocking(move || -> Result<(), SecretError> {
        let entry = keyring::Entry::new(&svc, &acct)
            .map_err(|e| SecretError::KeychainBackend(e.to_string()))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::KeychainBackend(e.to_string())),
        }
    })
    .await
    .map_err(|e| SecretError::KeychainBackend(format!("join: {e}")))?
}
```

**Step 3: Run.**

Run: `cargo test -p mur-common secret:: 2>&1 | tail -10`
Expected: All secret tests still pass + 2 new ones (15 total).

**Step 4: Commit.**

```bash
git add mur-common/src/secret.rs
git commit -m "$(cat <<'EOF'
feat(common): keychain_set/delete helpers for write-side ops

Used by `mur agent secret set` and the GUI's set_secret command.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Task 1.9 — Workspace gate

**Run:**

```bash
cargo build --workspace 2>&1 | tail -5
cargo clippy --workspace -- -D warnings 2>&1 | tail -20
cargo fmt --check 2>&1 | tail -5
cargo test --workspace 2>&1 | grep -E "test result:" | head -40
```

Expected: clean build, no clippy warnings, fmt clean, all `test result: ok` lines (count should be `baseline + 15`).

**No commit needed unless fmt rewrites something** — in that case `cargo fmt && git add -u && git commit -m "style: cargo fmt"`.

**End of PR-1.** Push branch (`git push -u origin feat/model-registry-and-secret-refs`) and open PR titled `feat(common): SecretRef abstraction (PR-1/5)` with body summarising tasks 1.1–1.9.

---

# PR-2 — `mur-common::model` + runtime resolution

## Task 2.1 — `ModelEntry` / `ModelRegistry` types

**Files:**
- Create: `mur-common/src/model.rs`
- Modify: `mur-common/src/lib.rs` (add `pub mod model;`)

**Step 1: Test.** In `mur-common/src/model.rs`:

```rust
//! Named model registry shared by all agents.
//!
//! On disk: ~/.mur/models.yaml. Schema:
//!
//! ```yaml
//! schema_version: 1
//! models:
//!   anthropic_opus_4_7:
//!     provider: anthropic
//!     model: claude-opus-4-7
//!     secret: env:ANTHROPIC_API_KEY
//!     capabilities: [chat, tools]
//! ```

use crate::secret::SecretRef;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelEntry {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelRegistry {
    pub schema_version: u32,
    #[serde(default)]
    pub models: BTreeMap<String, ModelEntry>,
}

impl Default for ModelRegistry {
    fn default() -> Self {
        Self { schema_version: 1, models: BTreeMap::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_registry() {
        let yaml = r#"
schema_version: 1
models:
  anthropic_opus_4_7:
    provider: anthropic
    model: claude-opus-4-7
    secret: env:ANTHROPIC_API_KEY
    capabilities: [chat, tools]
  ollama_llama3:
    provider: ollama
    model: llama3.2:3b
    base_url: http://127.0.0.1:11434
"#;
        let r: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(r.schema_version, 1);
        assert_eq!(r.models.len(), 2);
        let opus = r.models.get("anthropic_opus_4_7").unwrap();
        assert_eq!(opus.provider, "anthropic");
        assert_eq!(opus.secret, Some(SecretRef::Env("ANTHROPIC_API_KEY".into())));
        assert!(r.models["ollama_llama3"].secret.is_none());
    }

    #[test]
    fn round_trip_preserves_shape() {
        let mut r = ModelRegistry::default();
        r.models.insert("foo".into(), ModelEntry {
            provider: "anthropic".into(),
            model: "claude-opus-4-7".into(),
            base_url: None,
            secret: Some(SecretRef::Keychain { service: "mur".into(), account: "anthropic".into() }),
            capabilities: vec!["chat".into()],
            params: serde_json::Value::Null,
        });
        let s = serde_yaml_ng::to_string(&r).unwrap();
        let parsed: ModelRegistry = serde_yaml_ng::from_str(&s).unwrap();
        assert_eq!(r, parsed);
    }

    #[test]
    fn rejects_unknown_secret_scheme() {
        let yaml = r#"
schema_version: 1
models:
  bad:
    provider: x
    model: y
    secret: bogus:value
"#;
        let r: Result<ModelRegistry, _> = serde_yaml_ng::from_str(yaml);
        assert!(r.is_err(), "should reject unknown scheme");
    }
}
```

**Step 2: Register module.** In `mur-common/src/lib.rs`, `pub mod model;`.

**Step 3: Run.**

Run: `cargo test -p mur-common model:: 2>&1 | tail -10`
Expected: `3 passed`.

**Step 4: Commit.**

```bash
git add mur-common/src/model.rs mur-common/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(common): ModelEntry + ModelRegistry types

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Task 2.2 — `ModelRegistry::load` / `save` + I/O helpers

**File:** `mur-common/src/model.rs`

**Step 1: Test.**

```rust
#[cfg(test)]
mod io_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_returns_empty_when_file_missing() {
        let dir = tempdir().unwrap();
        let r = ModelRegistry::load_from(&dir.path().join("nope.yaml")).unwrap();
        assert_eq!(r.models.len(), 0);
        assert_eq!(r.schema_version, 1);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("models.yaml");
        let mut r = ModelRegistry::default();
        r.models.insert("x".into(), ModelEntry {
            provider: "ollama".into(), model: "llama3.2:3b".into(),
            base_url: None, secret: None, capabilities: vec![], params: serde_json::Value::Null,
        });
        r.save_to(&p).unwrap();
        let r2 = ModelRegistry::load_from(&p).unwrap();
        assert_eq!(r, r2);
    }

    #[test]
    fn save_uses_atomic_rename() {
        // Verify the temp file is gone after save.
        let dir = tempdir().unwrap();
        let p = dir.path().join("models.yaml");
        ModelRegistry::default().save_to(&p).unwrap();
        let temp_pattern = dir.path().join("models.yaml.tmp");
        assert!(!temp_pattern.exists(), "atomic temp left behind");
    }
}
```

**Step 2: Implement.**

```rust
use std::path::Path;

impl ModelRegistry {
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let body = std::fs::read_to_string(path)?;
        if body.trim().is_empty() {
            return Ok(Self::default());
        }
        Ok(serde_yaml_ng::from_str(&body)?)
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_yaml_ng::to_string(self)?;
        let tmp = path.with_extension("yaml.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn default_path() -> anyhow::Result<std::path::PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
        Ok(home.join(".mur/models.yaml"))
    }
}
```

**Step 3: Run.**

Run: `cargo test -p mur-common model::io_tests 2>&1 | tail -10`
Expected: `3 passed`.

**Step 4: Commit.**

```bash
git add mur-common/src/model.rs
git commit -m "$(cat <<'EOF'
feat(common): ModelRegistry load/save with atomic rename

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Task 2.3 — `AgentProfile` adds optional `model_ref`

**File:** `mur-common/src/agent.rs`

**Step 1: Test (round-trip).** In a new `#[cfg(test)] mod model_ref_tests` at the bottom of agent.rs:

```rust
#[cfg(test)]
mod model_ref_tests {
    use super::*;

    #[test]
    fn parses_profile_with_model_ref() {
        let yaml = r#"
schema: 1
id: 019dd878-758e-7741-bd24-12e475154a6e
name: kelp
display_name: Kelp
version: 0.1.0
persona: { category: custom, traits: { tone: concise, risk: cautious, verbosity: medium } }
sys_prompt_file: sys_prompt.md
model: { provider: anthropic, name: claude-opus-4-7, params: {} }
model_ref: anthropic_opus_4_7
mcp_servers: []
skills: []
transport: { stdio: true, socket: { path: agent.sock } }
communication: {}
entitlements:
  network:
    inbound: { ports: [] }
    outbound: { mode: restricted, allow_hosts: [], protocols: [tcp], resolve_dns: { mode: system, servers: [] } }
  filesystem: { read: [], write: [], deny: [] }
  processes: { spawn: { mode: allowlist, allowed: [] } }
  syscalls: { mode: default, extra_deny: [] }
  limits: {}
notifications: {}
retry: { policy: { strategy: exponential, max_attempts: 3 } }
lifecycle: {}
created_at: '2026-04-29T17:01:00Z'
updated_at: '2026-04-29T17:01:00Z'
"#;
        let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(p.model_ref.as_deref(), Some("anthropic_opus_4_7"));
        assert_eq!(p.model.provider, "anthropic"); // legacy still parsed
    }

    #[test]
    fn legacy_profile_without_model_ref_still_parses() {
        let yaml = include_str!("../../mur-agent-runtime/tests/fixtures/profile_minimal.yaml");
        let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(p.model_ref.is_none());
    }
}
```

(If the test fixture doesn't exist, replace the second test with an inline minimal yaml that mirrors what `mur agent create` writes today — copy a profile.yaml from `~/.mur/agents/<some-existing>/profile.yaml` into `mur-common/tests/fixtures/profile_minimal.yaml` first and `git add` it.)

**Step 2: Add the field.** In `AgentProfile`:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub model_ref: Option<String>,
```

**Step 3: Run.**

Run: `cargo test -p mur-common agent:: 2>&1 | tail -10`
Expected: existing tests still pass + 2 new ones.

**Step 4: Commit.**

```bash
git add mur-common/src/agent.rs mur-common/tests/fixtures/ 2>/dev/null || true
git commit -m "$(cat <<'EOF'
feat(common): AgentProfile.model_ref optional field (registry pointer)

Legacy `model:` block keeps working; runtime prefers model_ref when set.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Task 2.4 — Resolve `model_ref` in runtime supervisor

**File:** `mur-agent-runtime/src/supervisor.rs`

**Step 1: Add a helper that returns the effective `ModelEntry` for a profile.** Near the top of `supervisor.rs`, add:

```rust
fn resolve_model_entry(profile: &mur_common::agent::AgentProfile)
    -> anyhow::Result<mur_common::model::ModelEntry>
{
    use mur_common::model::{ModelEntry, ModelRegistry};
    if let Some(name) = profile.model_ref.as_deref() {
        let path = ModelRegistry::default_path()?;
        let reg = ModelRegistry::load_from(&path)
            .with_context(|| format!("load registry {}", path.display()))?;
        let entry = reg.models.get(name).ok_or_else(|| anyhow::anyhow!(
            "model_ref {name:?} not found in {}", path.display()
        ))?;
        Ok(entry.clone())
    } else {
        // Legacy fallback: synthesize a transient ModelEntry from the inline block.
        Ok(ModelEntry {
            provider: profile.model.provider.clone(),
            model: profile.model.name.clone(),
            base_url: None,
            secret: None,
            capabilities: vec![],
            params: serde_json::to_value(&profile.model.params).unwrap_or(serde_json::Value::Null),
        })
    }
}
```

(Add `use anyhow::Context;` if not already imported.)

**Step 2: Wire it into provider dispatch.** Where the existing `match profile.inner.model.provider.as_str()` is (around line 154), refactor to:

```rust
let entry = resolve_model_entry(&profile.inner)
    .with_context(|| "resolve model for runtime")?;
let secret_value: Option<secrecy::SecretString> = match &entry.secret {
    Some(s) => Some(s.resolve().await?),
    None => None,
};
match entry.provider.as_str() {
    "ollama" => { /* unchanged, but use entry.model + entry.base_url instead of env */ }
    "anthropic" => {
        let key = secret_value.ok_or_else(|| anyhow::anyhow!("anthropic provider requires a secret"))?;
        let client: Arc<dyn LlmClient> = Arc::new(
            AnthropicClient::from_secret_string(&key, entry.model.clone())
        );
        Arc::new(TaskRunner::with_llm(client).with_system_prompt(profile.system_prompt.clone()))
    }
    "openai" => { /* analogous to anthropic */ }
    other => { tracing::warn!(provider=%other, "no LLM client; echo"); Arc::new(TaskRunner::new_stub_echo()) }
}
```

You will also need a sibling constructor on each LLM client. For `AnthropicClient`:

```rust
// mur-agent-runtime/src/llm/anthropic.rs
impl AnthropicClient {
    pub fn from_secret_string(key: &secrecy::SecretString, model: String) -> Self {
        use secrecy::ExposeSecret;
        let base = std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.into());
        Self::new(base, key.expose_secret().to_string(), model)
    }
}
```

Add `from_secret_string` to OpenAI and Ollama analogously.

**Step 3: Tests.** Add an integration test under `mur-agent-runtime/tests/` named `model_resolution.rs`:

```rust
//! Confirm legacy inline `model:` and new `model_ref:` both resolve.
use mur_common::agent::AgentProfile;
use mur_common::model::{ModelEntry, ModelRegistry};

#[tokio::test]
async fn legacy_profile_resolves_inline() {
    let yaml = include_str!("fixtures/profile_legacy.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    let entry = mur_agent_runtime::test_helpers::resolve_model_entry(&p).unwrap();
    assert_eq!(entry.provider, "anthropic");
}

#[tokio::test]
async fn model_ref_resolves_from_registry() {
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("HOME", dir.path()); }
    std::fs::create_dir_all(dir.path().join(".mur")).unwrap();
    let mut reg = ModelRegistry::default();
    reg.models.insert("test_model".into(), ModelEntry {
        provider: "ollama".into(),
        model: "llama3.2:3b".into(),
        base_url: None, secret: None, capabilities: vec![], params: serde_json::Value::Null,
    });
    reg.save_to(&dir.path().join(".mur/models.yaml")).unwrap();
    let yaml_with_ref = include_str!("fixtures/profile_with_model_ref.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml_with_ref).unwrap();
    let entry = mur_agent_runtime::test_helpers::resolve_model_entry(&p).unwrap();
    assert_eq!(entry.model, "llama3.2:3b");
}
```

Expose `resolve_model_entry` via a `pub mod test_helpers;` gated on `#[cfg(any(test, feature = "test-helpers"))]` so external integration tests can call it.

**Step 4: Run.**

Run: `cargo test -p mur-agent-runtime model_resolution 2>&1 | tail -15`
Expected: `2 passed`.

**Step 5: Commit.**

```bash
git add mur-agent-runtime/src mur-agent-runtime/tests/
git commit -m "$(cat <<'EOF'
feat(runtime): resolve model_ref from ~/.mur/models.yaml; legacy fallback

Adds resolve_model_entry() that prefers profile.model_ref (looks up
~/.mur/models.yaml entry by name) and falls back to the inline `model:`
block when absent. Each LLM client gains a from_secret_string()
constructor so the supervisor pulls the key out of SecretRef once and
hands it to the client.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Task 2.5 — End-to-end smoke: registry-driven echo run

**Step 1: Write a shell script.** Create `scripts/e2e/p1-model-registry-smoke.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
TMP=$(mktemp -d)
trap "rm -rf $TMP" EXIT
export HOME=$TMP
mkdir -p $HOME/.mur

cat > $HOME/.mur/models.yaml <<'YAML'
schema_version: 1
models:
  echo_dev:
    provider: ollama
    model: llama3.2:3b
    base_url: http://127.0.0.1:11434
YAML

cargo run --quiet --bin mur -- agent create echo_a --no-interactive --provider ollama --model llama3.2:3b
# Manually rewrite profile to use model_ref instead of the inline block
sed -i.bak '/^model:/,/^[a-z]/{/^model:/d;/^  /d}' $HOME/.mur/agents/echo_a/profile.yaml
echo 'model_ref: echo_dev' >> $HOME/.mur/agents/echo_a/profile.yaml
echo 'model: { provider: ollama, name: llama3.2:3b, params: {} }' >> $HOME/.mur/agents/echo_a/profile.yaml

# Force echo runner so we don't need an actual LLM
MUR_AGENT_FORCE_ECHO=1 cargo run --quiet --bin mur-agent-runtime -- --profile echo_a &
RUNTIME_PID=$!
sleep 1

OUT=$(cargo run --quiet --bin mur -- agent send echo_a '{"role":"user","parts":[{"kind":"text","text":"ping"}]}')
echo "$OUT" | grep -q "echo: ping" || { echo "FAIL: $OUT"; kill $RUNTIME_PID; exit 1; }

cargo run --quiet --bin mur -- agent stop echo_a
wait $RUNTIME_PID 2>/dev/null || true
echo "OK: registry-driven echo smoke passed"
```

`chmod +x scripts/e2e/p1-model-registry-smoke.sh`.

**Step 2: Run.**

```bash
bash scripts/e2e/p1-model-registry-smoke.sh
```

Expected: `OK: registry-driven echo smoke passed`.

**Step 3: Commit.**

```bash
git add scripts/e2e/p1-model-registry-smoke.sh
git commit -m "$(cat <<'EOF'
test(e2e): registry-driven echo smoke (model_ref resolution)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

**End of PR-2.** Push, open PR titled `feat(runtime): model registry resolution (PR-2/5)`.

---

# PR-3 — CLI verbs

## Task 3.1 — `mur model add` / `list` / `show` / `remove`

**Files:**
- Create: `mur-core/src/cmd/model.rs`
- Modify: `mur-core/src/cmd/mod.rs` (add `pub mod model;`)
- Modify: `mur-core/src/cli.rs` (or wherever the top-level clap enum is) — add `Model(ModelArgs)` variant.

**Step 1: Sketch clap subcommand.** In `mur-core/src/cmd/model.rs`:

```rust
use clap::{Args, Subcommand};
use mur_common::model::{ModelEntry, ModelRegistry};
use mur_common::secret::SecretRef;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct ModelArgs {
    #[command(subcommand)]
    pub cmd: ModelCmd,
}

#[derive(Subcommand, Debug)]
pub enum ModelCmd {
    /// Add or replace a model entry.
    Add {
        name: String,
        #[arg(long)] provider: String,
        #[arg(long)] model: String,
        #[arg(long)] base_url: Option<String>,
        /// Secret ref (e.g. env:ANTHROPIC_API_KEY, keychain:mur/anthropic).
        #[arg(long)] secret: Option<String>,
        #[arg(long, value_delimiter = ',')] capabilities: Vec<String>,
    },
    List,
    Show { name: String },
    Remove { name: String },
}

pub fn run(args: ModelArgs) -> anyhow::Result<()> {
    let path = ModelRegistry::default_path()?;
    let mut reg = ModelRegistry::load_from(&path)?;
    match args.cmd {
        ModelCmd::Add { name, provider, model, base_url, secret, capabilities } => {
            let secret_ref = secret.map(|s| s.parse::<SecretRef>()).transpose()?;
            reg.models.insert(name.clone(), ModelEntry {
                provider, model, base_url, secret: secret_ref,
                capabilities, params: serde_json::Value::Null,
            });
            reg.save_to(&path)?;
            println!("Added model {name} → {}", path.display());
        }
        ModelCmd::List => {
            if reg.models.is_empty() { println!("(no models registered)"); }
            for (n, e) in &reg.models {
                println!("{n}\t{}\t{}", e.provider, e.model);
            }
        }
        ModelCmd::Show { name } => {
            let e = reg.models.get(&name).ok_or_else(|| anyhow::anyhow!("not found: {name}"))?;
            print!("{}", serde_yaml_ng::to_string(e)?);
        }
        ModelCmd::Remove { name } => {
            if reg.models.remove(&name).is_some() {
                reg.save_to(&path)?;
                println!("Removed {name}");
            } else {
                anyhow::bail!("not found: {name}");
            }
        }
    }
    Ok(())
}
```

**Step 2: Wire into top-level clap.** In `cli.rs` (or wherever):

```rust
Model(crate::cmd::model::ModelArgs),
```

and in the dispatch:

```rust
Cmd::Model(args) => crate::cmd::model::run(args)?,
```

**Step 3: Smoke test it manually.**

```bash
cargo run --quiet --bin mur -- model add anthropic_opus_4_7 \
  --provider anthropic --model claude-opus-4-7 --secret env:ANTHROPIC_API_KEY
cargo run --quiet --bin mur -- model list
cargo run --quiet --bin mur -- model show anthropic_opus_4_7
cargo run --quiet --bin mur -- model remove anthropic_opus_4_7
```

Expected: each step prints something sensible; the remove succeeds without error.

**Step 4: Add an integration test.** `mur-core/tests/cmd_model.rs`:

```rust
use std::process::Command;

#[test]
fn add_list_remove_round_trip() {
    let bin = env!("CARGO_BIN_EXE_mur");
    let dir = tempfile::tempdir().unwrap();
    let env = [("HOME", dir.path().to_string_lossy().to_string())];
    let output = |args: &[&str]| -> String {
        let mut cmd = Command::new(bin);
        cmd.envs(env.iter().cloned()).args(args);
        let o = cmd.output().unwrap();
        assert!(o.status.success(), "{:?}: {}", args, String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    };
    output(&["model", "add", "x", "--provider", "ollama", "--model", "llama3:3b"]);
    let list = output(&["model", "list"]);
    assert!(list.contains("x\tollama\tllama3:3b"));
    output(&["model", "remove", "x"]);
    let list2 = output(&["model", "list"]);
    assert!(list2.contains("(no models registered)"));
}
```

**Step 5: Run.**

```bash
cargo test -p mur-core cmd_model 2>&1 | tail -10
```

Expected: `1 passed`.

**Step 6: Commit.**

```bash
git add mur-core/src/cmd/model.rs mur-core/src/cmd/mod.rs mur-core/src/cli.rs mur-core/tests/cmd_model.rs
git commit -m "$(cat <<'EOF'
feat(cli): mur model {add,list,show,remove}

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Task 3.2 — `mur model migrate`

**File:** `mur-core/src/cmd/model.rs`

**Step 1: Add the variant.** Append to `ModelCmd`:

```rust
Migrate {
    #[arg(long)] dry_run: bool,
},
```

**Step 2: Implement.** Add a function:

```rust
fn cmd_migrate(dry_run: bool) -> anyhow::Result<()> {
    use mur_common::agent::AgentProfile;
    let agents_dir = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no HOME"))?
        .join(".mur/agents");
    let registry_path = ModelRegistry::default_path()?;
    let mut reg = ModelRegistry::load_from(&registry_path)?;
    let mut migrated_agents: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&agents_dir)? {
        let entry = entry?;
        let pyaml = entry.path().join("profile.yaml");
        if !pyaml.exists() { continue; }
        let body = std::fs::read_to_string(&pyaml)?;
        let mut profile: AgentProfile = serde_yaml_ng::from_str(&body)?;
        if profile.model_ref.is_some() { continue; }
        let name = format!("{}_{}",
            profile.model.provider,
            profile.model.name.replace(['-', ':', '.'], "_"));
        reg.models.entry(name.clone()).or_insert_with(|| ModelEntry {
            provider: profile.model.provider.clone(),
            model: profile.model.name.clone(),
            base_url: None, secret: None, capabilities: vec![],
            params: serde_json::Value::Null,
        });
        profile.model_ref = Some(name.clone());
        migrated_agents.push(format!("{} → {name}", profile.name));
        if !dry_run {
            let new = serde_yaml_ng::to_string(&profile)?;
            let tmp = pyaml.with_extension("yaml.tmp");
            std::fs::write(&tmp, new)?;
            std::fs::rename(&tmp, &pyaml)?;
        }
    }
    if !dry_run {
        reg.save_to(&registry_path)?;
    }
    println!("{} agents would migrate:", migrated_agents.len());
    for line in migrated_agents { println!("  {line}"); }
    if dry_run { println!("(dry run — pass without --dry-run to apply)"); }
    Ok(())
}
```

Wire into `run()` match arm.

**Step 3: Test.** Add to `mur-core/tests/cmd_model.rs`:

```rust
#[test]
fn migrate_dry_run_then_apply() {
    let bin = env!("CARGO_BIN_EXE_mur");
    let dir = tempfile::tempdir().unwrap();
    let env = [("HOME", dir.path().to_string_lossy().to_string())];
    Command::new(bin).envs(env.iter().cloned())
        .args(["agent", "create", "a1", "--no-interactive", "--provider", "ollama", "--model", "llama3:3b"])
        .output().unwrap();
    let dry = Command::new(bin).envs(env.iter().cloned())
        .args(["model", "migrate", "--dry-run"]).output().unwrap();
    assert!(String::from_utf8_lossy(&dry.stdout).contains("dry run"));
    let apply = Command::new(bin).envs(env.iter().cloned())
        .args(["model", "migrate"]).output().unwrap();
    assert!(apply.status.success());
    let pyaml = dir.path().join(".mur/agents/a1/profile.yaml");
    let body = std::fs::read_to_string(&pyaml).unwrap();
    assert!(body.contains("model_ref:"), "profile not rewritten: {body}");
}
```

**Step 4: Run + commit.**

```bash
cargo test -p mur-core cmd_model 2>&1 | tail -10
git add -u
git commit -m "$(cat <<'EOF'
feat(cli): mur model migrate (lift inline model: into registry, opt-in)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Task 3.3 — `mur agent secret {set,list,delete}`

**Files:**
- Modify: `mur-core/src/cmd/agent.rs` (add `Secret(SecretArgs)` subvariant under the existing `mur agent ...` enum)
- Implement in same file.

**Step 1: Add clap shape.** In whichever `enum AgentCmd` is defined:

```rust
Secret(SecretArgs),
```

and:

```rust
#[derive(Args, Debug)]
pub struct SecretArgs {
    pub agent: String,
    #[command(subcommand)]
    pub cmd: SecretCmd,
}

#[derive(Subcommand, Debug)]
pub enum SecretCmd {
    /// Write a secret to the OS keychain (service=mur-agent, account=<agent>/<key>).
    Set { key: String, value: Option<String> },
    /// List which secret refs the agent uses.
    List,
    /// Delete a keychain entry by key name.
    Delete { key: String },
}
```

**Step 2: Implement.** Helper `cmd_secret(args)`:

```rust
async fn cmd_secret(args: SecretArgs) -> anyhow::Result<()> {
    use mur_common::secret::{keychain_set, keychain_delete, SecretRef};
    let svc = "mur-agent";
    match args.cmd {
        SecretCmd::Set { key, value } => {
            let val = match value {
                Some(v) => v,
                None => {
                    eprint!("Enter value for {key} (input hidden): ");
                    let typed = rpassword::read_password()?;
                    typed
                }
            };
            let acct = format!("{}/{key}", args.agent);
            keychain_set(svc, &acct, &val).await?;
            println!("Wrote {svc}/{acct}");
        }
        SecretCmd::List => {
            // Read profile + registry to figure out which refs apply.
            let profile = load_agent_profile(&args.agent)?;
            if let Some(name) = profile.model_ref.as_deref() {
                let reg = mur_common::model::ModelRegistry::load_from(
                    &mur_common::model::ModelRegistry::default_path()?)?;
                if let Some(entry) = reg.models.get(name) {
                    if let Some(s) = &entry.secret {
                        let ok = s.check().await;
                        println!("{} ({}) — {}", s, name, if ok { "✓ set" } else { "✗ not set" });
                    } else {
                        println!("{name} has no secret");
                    }
                }
            } else {
                println!("agent uses inline model — no registry secret");
            }
        }
        SecretCmd::Delete { key } => {
            let acct = format!("{}/{key}", args.agent);
            keychain_delete(svc, &acct).await?;
            println!("Deleted {svc}/{acct}");
        }
    }
    Ok(())
}
```

Add `rpassword = "7"` to `mur-core/Cargo.toml` deps.

**Step 3: Wire into the existing `agent` dispatcher.** Match on the new `AgentCmd::Secret(args) => tokio_runtime.block_on(cmd_secret(args))?` (or call `.await` if dispatcher is already async).

**Step 4: Smoke test.**

```bash
HOME=$(mktemp -d) cargo run --quiet --bin mur -- agent create kelp --no-interactive --provider ollama --model llama3:3b
cargo run --quiet --bin mur -- agent secret kelp set ANTHROPIC_API_KEY foo
cargo run --quiet --bin mur -- agent secret kelp list
cargo run --quiet --bin mur -- agent secret kelp delete ANTHROPIC_API_KEY
```

(On the dev machine the second command will write to the actual macOS Keychain — that's fine, just delete it after.)

**Step 5: Commit.**

```bash
git add mur-core/src/cmd/agent.rs mur-core/Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
feat(cli): mur agent secret {set,list,delete}

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

**End of PR-3.** Push, open PR.

---

# PR-4 — GUI sidecar env injection

**Goal:** When `Kelp.app` is launched from Finder, the Tauri main resolves the active model's secret and injects it via `Command::env()` into the sidecar — so the sidecar's existing `from_env()` path picks it up. No more "GUI launch falls back to echo because shell env not inherited."

## Task 4.1 — Resolve active model + secret in sidecar manager

**File:** `mur-agent-gui/src-tauri/src/sidecar.rs`

**Step 1: Read current sidecar `Command` builder.** Find the `start()` (or equivalent) method that constructs a `tauri::process::Command` for the sidecar runtime binary.

**Step 2: Add an async helper.** Above `start()`:

```rust
async fn resolve_secrets_for_agent(agent_name: &str) -> Vec<(String, String)> {
    use mur_common::agent::AgentProfile;
    use mur_common::model::ModelRegistry;

    let home = match dirs::home_dir() { Some(h) => h, None => return vec![] };
    let pyaml = home.join(format!(".mur/agents/{agent_name}/profile.yaml"));
    let body = match std::fs::read_to_string(&pyaml) { Ok(b) => b, Err(_) => return vec![] };
    let profile: AgentProfile = match serde_yaml_ng::from_str(&body) { Ok(p) => p, Err(_) => return vec![] };
    let Some(model_ref) = profile.model_ref else { return vec![] };
    let reg = match ModelRegistry::load_from(&ModelRegistry::default_path().unwrap_or_default())
        { Ok(r) => r, Err(_) => return vec![] };
    let Some(entry) = reg.models.get(&model_ref) else { return vec![] };
    let Some(secret) = &entry.secret else { return vec![] };
    let resolved = match secret.resolve().await {
        Ok(s) => s,
        Err(e) => { tracing::warn!(error=%e, "resolve secret for sidecar"); return vec![]; }
    };
    use secrecy::ExposeSecret;
    let env_var = match entry.provider.as_str() {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai"    => "OPENAI_API_KEY",
        _ => return vec![],
    };
    vec![(env_var.to_string(), resolved.expose_secret().to_string())]
}
```

**Step 3: Wire into `start()`.** Just before the `Command::new(...)` is built, await the helper and `for (k, v) in env_vars { cmd.env(k, v); }`. `tracing::info!` the env keys (NEVER the values).

**Step 4: Build + manual test.**

```bash
cd mur-agent-gui/src-tauri && cargo build 2>&1 | tail -5
cd ../.. && bash scripts/e2e/p1-export-gui.sh   # quick mode, just confirm the build succeeds
```

Expected: GUI builds clean. (E2E run that proves "Finder launch → real LLM" requires a real API key + signed app; that's the manual acceptance check below.)

**Step 5: Commit.**

```bash
git add mur-agent-gui/src-tauri/src/sidecar.rs
git commit -m "$(cat <<'EOF'
feat(gui): resolve active model secret and inject via Command::env()

Tauri main reads the agent's profile.model_ref → ~/.mur/models.yaml,
resolves entry.secret via mur-common::secret, and passes the value as
an env var (ANTHROPIC_API_KEY / OPENAI_API_KEY) to the sidecar.
Sidecar's existing from_env() path picks it up unchanged.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Task 4.2 — Manual acceptance

(Documented for the human reviewer; not automatable.)

```
1. mur agent create kelp ... (using --provider anthropic --model claude-opus-4-7)
2. mur model add anthropic_test --provider anthropic --model claude-opus-4-7 --secret keychain:mur-agent/kelp/ANTHROPIC_API_KEY
3. Edit ~/.mur/agents/kelp/profile.yaml — add `model_ref: anthropic_test`.
4. mur agent secret kelp set ANTHROPIC_API_KEY <real key>
5. mur agent export kelp --format gui --out ~/Desktop/Kelp.app --skip-notarize
6. xattr -dr com.apple.quarantine ~/Desktop/Kelp.app && open ~/Desktop/Kelp.app
7. From the GUI's Status tab, send a real chat (or via `mur agent send` from a clean shell with NO env).
8. Expected: real Claude reply (no `echo:` prefix).
```

Add this script to `scripts/e2e/p1-gui-secret-injection.md` (markdown rather than .sh because it requires a real signed key + manual Finder click).

**Step 1: Write the doc.** Create file with the steps above.

**Step 2: Commit.**

```bash
git add scripts/e2e/p1-gui-secret-injection.md
git commit -m "$(cat <<'EOF'
docs(e2e): manual acceptance for GUI secret injection

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

**End of PR-4.** Push, open PR.

---

# PR-5 — GUI Model tab

**Scope: B from the design.** List registry, switch active model, edit secret via modal. Cannot create / delete model entries from GUI.

## Task 5.1 — New Tauri commands

**File:** `mur-agent-gui/src-tauri/src/commands.rs`

**Step 1: Add command shells.**

```rust
use mur_common::model::{ModelRegistry, ModelEntry};
use mur_common::secret::{SecretRef, keychain_set};

#[derive(serde::Serialize)]
pub struct ModelEntryView {
    pub name: String,
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    pub secret_ref: Option<String>,
    pub secret_status: Option<bool>, // None = no secret needed; Some(true/false) = check result
    pub capabilities: Vec<String>,
}

#[tauri::command]
pub async fn list_models() -> Result<Vec<ModelEntryView>, String> {
    let reg = ModelRegistry::load_from(&ModelRegistry::default_path().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(reg.models.len());
    for (name, e) in &reg.models {
        let secret_status = match &e.secret {
            Some(s) => Some(s.check().await),
            None => None,
        };
        out.push(ModelEntryView {
            name: name.clone(),
            provider: e.provider.clone(),
            model: e.model.clone(),
            base_url: e.base_url.clone(),
            secret_ref: e.secret.as_ref().map(|s| s.to_string()),
            secret_status,
            capabilities: e.capabilities.clone(),
        });
    }
    Ok(out)
}

#[tauri::command]
pub fn get_active_model_ref() -> Result<Option<String>, String> {
    let agent = agent_name();
    let pyaml = dirs::home_dir().ok_or("no HOME".to_string())?
        .join(format!(".mur/agents/{agent}/profile.yaml"));
    let body = std::fs::read_to_string(&pyaml).map_err(|e| e.to_string())?;
    let p: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(&body).map_err(|e| e.to_string())?;
    Ok(p.model_ref)
}

#[tauri::command]
pub fn set_active_model_ref(name: String) -> Result<(), String> {
    let agent = agent_name();
    let pyaml = dirs::home_dir().ok_or("no HOME".to_string())?
        .join(format!(".mur/agents/{agent}/profile.yaml"));
    let body = std::fs::read_to_string(&pyaml).map_err(|e| e.to_string())?;
    let mut p: mur_common::agent::AgentProfile =
        serde_yaml_ng::from_str(&body).map_err(|e| e.to_string())?;
    p.model_ref = Some(name);
    let new = serde_yaml_ng::to_string(&p).map_err(|e| e.to_string())?;
    let tmp = pyaml.with_extension("yaml.tmp");
    std::fs::write(&tmp, new).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &pyaml).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn set_secret(secret: String, value: String) -> Result<(), String> {
    let s: SecretRef = secret.parse().map_err(|e: mur_common::secret::SecretError| e.to_string())?;
    match s {
        SecretRef::Keychain { service, account } => {
            keychain_set(&service, &account, &value).await.map_err(|e| e.to_string())
        }
        SecretRef::Env(_) | SecretRef::File(_) | SecretRef::Cmd(_) => {
            Err("set_secret only writes to keychain refs".into())
        }
    }
}
```

**Step 2: Register in main.rs invoke_handler.** Append:

```rust
commands::list_models,
commands::get_active_model_ref,
commands::set_active_model_ref,
commands::set_secret,
```

**Step 3: Build.**

Run: `cd mur-agent-gui/src-tauri && cargo build 2>&1 | tail -5`
Expected: clean.

**Step 4: Commit.**

```bash
git add mur-agent-gui/src-tauri/src/commands.rs mur-agent-gui/src-tauri/src/main.rs
git commit -m "$(cat <<'EOF'
feat(gui): Tauri commands for model registry + secret writes

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Task 5.2 — Frontend Model tab

**Files:**
- Create: `mur-agent-gui/ui/src/tabs/Model.tsx`
- Modify: `mur-agent-gui/ui/src/App.tsx` (add to TABS, render ModelTab)
- Modify: `mur-agent-gui/ui/src/lib/api.ts` (add invoke wrappers + types)

**Step 1: api.ts additions.** Append:

```ts
export interface ModelEntryView {
  name: string;
  provider: string;
  model: string;
  base_url: string | null;
  secret_ref: string | null;
  secret_status: boolean | null; // null = no secret; true/false = check
  capabilities: string[];
}
export const listModels = () => invoke<ModelEntryView[]>("list_models");
export const getActiveModelRef = () => invoke<string | null>("get_active_model_ref");
export const setActiveModelRef = (name: string) => invoke<void>("set_active_model_ref", { name });
export const setSecret = (secret: string, value: string) =>
  invoke<void>("set_secret", { secret, value });
```

**Step 2: Model.tsx.**

```tsx
import { useEffect, useState } from "react";
import {
  listModels, getActiveModelRef, setActiveModelRef, setSecret,
  type ModelEntryView,
} from "../lib/api";

export default function ModelTab() {
  const [entries, setEntries] = useState<ModelEntryView[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const reload = () => {
    Promise.all([listModels(), getActiveModelRef()])
      .then(([m, a]) => { setEntries(m); setActive(a); })
      .catch((e) => setError(String(e)));
  };
  useEffect(reload, []);

  if (error) return <pre>{error}</pre>;
  if (entries.length === 0) {
    return (
      <div className="p-4">
        <h2 className="text-lg font-semibold mb-2">Model</h2>
        <p className="text-sm">
          No models in registry. Add one via CLI:
        </p>
        <pre className="mt-2 text-xs">
          mur model add anthropic_opus_4_7 --provider anthropic --model claude-opus-4-7 --secret keychain:mur-agent/{"<agent>"}/ANTHROPIC_API_KEY
        </pre>
      </div>
    );
  }

  return (
    <div className="p-4 space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-lg font-semibold">Model</h2>
        <button className="text-xs px-2 py-1 border rounded" onClick={reload}>Reload</button>
      </div>
      <div className="text-sm">
        Active: <strong>{active ?? "(none — using legacy inline model)"}</strong>
      </div>
      <ul className="space-y-2">
        {entries.map((e) => (
          <li key={e.name} className="border rounded p-3"
            style={{ borderColor: "var(--color-border)" }}>
            <div className="flex items-center gap-2">
              <input
                type="radio"
                name="active-model"
                checked={active === e.name}
                onChange={() => setActiveModelRef(e.name).then(reload).catch((err) => setError(String(err)))}
              />
              <span className="font-medium">{e.name}</span>
              {e.secret_status === false && <span className="text-xs text-red-500">✗ secret not set</span>}
              {e.secret_status === true && <span className="text-xs text-green-500">✓ ready</span>}
              {e.secret_status === null && <span className="text-xs">no secret needed</span>}
            </div>
            <div className="text-xs mt-1" style={{ color: "var(--color-fg-secondary)" }}>
              {e.provider} / {e.model}
              {e.secret_ref && <> · {e.secret_ref}</>}
            </div>
            {e.secret_ref && e.secret_ref.startsWith("keychain:") && (
              <button className="text-xs mt-2 px-2 py-1 border rounded"
                onClick={() => setEditing(e.name)}>
                {e.secret_status ? "Update" : "Set"} secret
              </button>
            )}
          </li>
        ))}
      </ul>

      {editing && (
        <SecretModal
          entry={entries.find((e) => e.name === editing)!}
          onClose={() => { setEditing(null); reload(); }}
          onError={setError}
        />
      )}
    </div>
  );
}

function SecretModal({ entry, onClose, onError }: {
  entry: ModelEntryView;
  onClose: () => void;
  onError: (e: string) => void;
}) {
  const [value, setValue] = useState("");
  const [show, setShow] = useState(false);
  const submit = () => {
    if (!entry.secret_ref) return;
    setSecret(entry.secret_ref, value)
      .then(() => { setValue(""); onClose(); })
      .catch((e) => onError(String(e)));
  };
  return (
    <div className="fixed inset-0 bg-black/40 flex items-center justify-center z-10"
      style={{ background: "rgba(0,0,0,0.4)" }}>
      <div className="rounded p-4 w-96 shadow-lg"
        style={{ background: "var(--color-bg)", border: "1px solid var(--color-border)" }}>
        <h3 className="font-semibold mb-2">Set secret for {entry.name}</h3>
        <p className="text-xs mb-2" style={{ color: "var(--color-fg-secondary)" }}>
          Stored in: <code>{entry.secret_ref}</code>
        </p>
        <input
          type={show ? "text" : "password"}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          autoFocus
          className="w-full border rounded px-2 py-1 text-sm mb-2"
          style={{ borderColor: "var(--color-border)" }}
        />
        <label className="text-xs flex items-center gap-1">
          <input type="checkbox" checked={show} onChange={(e) => setShow(e.target.checked)} />
          show
        </label>
        <div className="flex gap-2 mt-3 justify-end">
          <button className="text-xs px-3 py-1 border rounded" onClick={onClose}>Cancel</button>
          <button className="text-xs px-3 py-1 rounded"
            style={{ background: "var(--color-accent)", color: "var(--color-accent-fg)" }}
            disabled={!value}
            onClick={submit}>Save</button>
        </div>
      </div>
    </div>
  );
}
```

**Step 3: Add tab to App.tsx.**

```tsx
import ModelTab from "./tabs/Model";
// In TABS, between prompt and skills:
{ id: "model", label: "Model" },
// In the render block:
{tab === "model" && <ModelTab />}
```

(also add `"model"` to `type TabId`.)

**Step 4: Build frontend + tauri.**

```bash
cd mur-agent-gui/ui && npm run build 2>&1 | tail -5
cd ../src-tauri && cargo build 2>&1 | tail -5
```

Expected: clean.

**Step 5: Commit.**

```bash
git add mur-agent-gui/ui/src/tabs/Model.tsx mur-agent-gui/ui/src/App.tsx mur-agent-gui/ui/src/lib/api.ts
git commit -m "$(cat <<'EOF'
feat(gui): Model tab — list registry, switch active, set secret

Tab is read-only on the registry shape (no add/remove); CRUD stays in
the CLI (mur model add/remove). For keychain: refs the user can write
the value directly from the modal — solves the "GUI launched from
Finder can't read shell env" problem without leaving the app.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

## Task 5.3 — Manual acceptance

1. Re-export Kelp.app: `mur agent export kelp --format gui --out ~/Desktop/Kelp.app --skip-notarize`.
2. Open from Finder, navigate to Model tab.
3. Confirm: list shows all registry entries, current `model_ref` is highlighted, secret_status badges look right.
4. Click "Update secret" on the active entry → enter the real Anthropic key → Save.
5. Send a chat from the Status tab (or via `mur agent send` from a fresh shell with NO `ANTHROPIC_API_KEY`).
6. Expected: real Claude reply.

Update `scripts/e2e/p1-gui-secret-injection.md` to point at this flow.

**Commit:**

```bash
git add scripts/e2e/p1-gui-secret-injection.md
git commit -m "$(cat <<'EOF'
docs(e2e): update GUI secret-injection acceptance to use Model tab

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

**End of PR-5.** Push, open PR.

---

# Final close-out

After all 5 PRs merge:

1. Update top-level `CLAUDE.md` "CLI Commands" + "Architecture" sections to mention `mur model …` and the registry/secret resolution path.
2. Append a section to `mur-agent-runtime/README.md` linking to the design + plan.
3. Tag a `-COMPLETE.md` next to this plan listing each PR's URL + the mapping back to the design's §1–§5 sections (matches existing convention from `2026-04-29-mur-agent-gui-export-plan-COMPLETE.md`).
4. Run `superpowers:finishing-a-development-branch` to clean up the worktree.

## What's deliberately out of scope

- Auto OAuth refresh (P2 follow-up).
- Lifting the secret/model crates to a published crate so mur-commander can drop its parallel implementation. Do that only when a real cross-process need lands.
- GUI CRUD on the registry (Add new model entry from GUI). YAGNI for v1; CLI is the canonical write path.
- `Literal` SecretRef variant. Tests use `env:` indirection.
