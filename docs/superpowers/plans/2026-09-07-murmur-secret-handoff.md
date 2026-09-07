# murmur secret handoff — implementation plan

> **Execute with `mur-executing-plans`** (in-context, task by task). No MUR
> delegation is set up for this branch.

**Spec:** `docs/superpowers/specs/2026-09-07-murmur-secret-handoff-design.md`
**Branch:** `feat/murmur-secret-handoff` (draft PR #1203)

## Goal

Let a user hand a credential to a running agent from the murmur TUI so that the
plaintext never enters the LLM context or the signed channel: the model sees
`$NAME`, the runtime injects the value into the bash tool's environment, and
every tool result is masked before it reaches the model.

## Architecture

A per-process `SecretVault` (name → `SecretString`) lives in
`mur-agent-runtime`. It is filled pre-seal from the agent's keychain slot
(`mur-agent/<agent>/<NAME>`, names listed in `profile.secrets`) and mutated at
runtime by two new A2A methods, `secret/set` and `secret/delete`. Three
consumers read it: the system-prompt assembler (names only), the bash tool
(env injection at spawn), and the tool-result path in `TaskRunner` (value
masking). The murmur TUI gains `/secret <KEY> [--delete]`, which reads the
value with the terminal handed over (hidden input), writes keychain + profile,
then dials `secret/set` on the running agent and reports each step separately.

## Tech stack

Rust 2024 · `secrecy 0.10` (`SecretString`, already a dependency of both
crates) · `rpassword 7` (already in `mur-core`) · A2A JSON-RPC over unix
socket (`mur_core::a2a_dial::dial_method`) · `cargo nextest`.

## Global constraints (from the spec — every task includes these)

- The model never sees a secret value. Only names go into prompts, logs,
  tracing, A2A responses, telemetry, and TUI messages.
- Values shorter than `SecretVault::MIN_LEN` (8) are rejected wherever a
  value enters (TUI and runtime handler).
- Secret names match `[A-Z_][A-Z0-9_]*`.
- Partial success is reported per step; keychain failure aborts before the
  runtime push; two successes print one line.
- Masking is defense in depth. Tests must include the documented ceiling
  (re-encoded values are not caught).
- Per-agent only: nothing propagates to fleet members or delegate specialists.
- CLAUDE.md rule 4: no source file over 800 lines. `task_runner.rs` and
  `cli/mod.rs` are already over; add the *minimum* lines there and keep new
  logic in new files.
- CLAUDE.md rule 7: user-visible brand is "MUR".
- Tests: `cargo nextest run -p <crate>` (not `cargo test`; see memory
  `project_mur_core_flaky_tests`). `mur-core` needs
  `MUR_WEB_DIST=$HOME/Projects/mur-web/dist` and
  `RUST_MIN_STACK=33554432` to build/test.
- Commit after every green step. Verify the branch before each commit as a
  separate tool call (`git branch --show-current` → `feat/murmur-secret-handoff`).

## File structure

| File | Change | Responsibility |
|---|---|---|
| `mur-common/src/agent.rs` | modify | `AgentProfile.secrets: Vec<String>` — names only |
| `mur-core/src/cmd/agent/lifecycle.rs`, `mur-core/src/cmd/agent_companion/connector.rs` | modify | add `secrets: Vec::new()` to the two exhaustive `AgentProfile` literals |
| `mur-agent-runtime/src/secrets.rs` | **new** | `SecretVault`: validate, store, `env_pairs`, `mask`, `prompt_fragment` |
| `mur-agent-runtime/src/lib.rs` | modify | `pub mod secrets;` |
| `mur-agent-runtime/src/protocol/methods/secret_set.rs` | **new** | `secret/set` + `secret/delete` A2A handlers |
| `mur-agent-runtime/src/protocol/methods/mod.rs` | modify | `pub mod secret_set;` |
| `mur-agent-runtime/src/supervisor.rs` | modify | pre-seal load into the vault; register both methods; pass vault to `prepare_runtime` |
| `mur-agent-runtime/src/supervisor_runner.rs` | modify | thread `Arc<SecretVault>` into `BashTool` and `TaskRunner` |
| `mur-agent-runtime/src/tools/bash.rs` | modify | `secrets` field + `with_secrets`; env injection at spawn |
| `mur-agent-runtime/src/tools/registry.rs` | modify | two test literals gain `secrets: None` |
| `mur-agent-runtime/src/task_runner.rs` | modify | `secrets` field + `with_secrets`; prompt fragment; mask at both execute sites |
| `mur-core/src/cmd/agent/cli/app.rs` | modify | `SlashCmd::Secret`, `parse_slash` arm, `App.pending_secret_prompt` |
| `mur-core/src/cmd/agent/cli/handover.rs` | modify | `read_hidden()` — hidden line read with the terminal handed over |
| `mur-core/src/cmd/agent/cli/secret_cmd.rs` | **new** | `/secret` handler: validate, keychain, profile, dial, report |
| `mur-core/src/cmd/agent/cli/mod.rs` | modify | HELP text, `help_name`, `one_of_each`, dispatch arm, main-loop prompt handling |
| `mur-core/src/cmd/agent/cli/complete.rs` | modify | `("secret", …)` completion entry |
| `CLAUDE.md` | modify | one clause in the `mur agent` CLI-surface line |

---

## Task 1 — `SecretVault` (runtime, pure)

**Interfaces — Produces:**

```rust
// mur-agent-runtime/src/secrets.rs
pub struct SecretVault { /* private */ }
pub const MIN_LEN: usize = 8;
pub const KEYCHAIN_SERVICE: &str = "mur-agent";
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SecretVaultError {
    #[error("secret name '{0}' must match [A-Z_][A-Z0-9_]*")] BadName(String),
    #[error("secret '{0}' is shorter than {MIN_LEN} characters")] TooShort(String),
}
pub fn valid_name(name: &str) -> bool;
impl SecretVault {
    pub fn new() -> Self;
    pub fn set(&self, name: &str, value: &str) -> Result<(), SecretVaultError>;
    pub fn remove(&self, name: &str) -> bool;
    pub fn names(&self) -> Vec<String>;                  // sorted
    pub fn env_pairs(&self) -> Vec<(String, String)>;    // exposes values — spawn only
    pub fn mask<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str>;
    pub fn prompt_fragment(&self) -> Option<String>;     // None when empty
}
impl Default for SecretVault
```

- [ ] **1.1 Write the failing tests.** Create `mur-agent-runtime/src/secrets.rs` with only the test module:

```rust
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
        assert_eq!(v.names(), vec!["ALPHA_KEY".to_string(), "ZED_TOKEN".to_string()]);
    }

    #[test]
    fn set_overwrites_and_remove_reports_presence() {
        let v = SecretVault::new();
        v.set("K", "first-value").unwrap();
        v.set("K", "second-value").unwrap();
        assert_eq!(v.env_pairs(), vec![("K".to_string(), "second-value".to_string())]);
        assert!(v.remove("K"));
        assert!(!v.remove("K"));
        assert!(v.env_pairs().is_empty());
    }

    #[test]
    fn mask_replaces_every_value_with_its_name_tag() {
        let v = SecretVault::new();
        v.set("GITEA_TOKEN", "d8b04a3cc632a5c8026cf5a810d36e292c603f99").unwrap();
        v.set("OTHER", "hunter2hunter2").unwrap();
        let out = v.mask("token=d8b04a3cc632a5c8026cf5a810d36e292c603f99 pw=hunter2hunter2 again d8b04a3cc632a5c8026cf5a810d36e292c603f99");
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
        v.set("GITEA_TOKEN", "d8b04a3cc632a5c8026cf5a810d36e292c603f99").unwrap();
        let f = v.prompt_fragment().unwrap();
        assert!(f.contains("$GITEA_TOKEN"), "{f}");
        assert!(!f.contains("d8b04a3c"), "{f}");
        assert!(f.contains("Authorization"), "the git guidance line is part of the fragment: {f}");
    }
}
```

  Add `pub mod secrets;` to `mur-agent-runtime/src/lib.rs` (alphabetical among the existing `pub mod` lines).
- [ ] **1.2 Watch it fail.** `cargo nextest run -p mur-agent-runtime secrets::` → compile error: `SecretVault` not found.
- [ ] **1.3 Implement.** Insert above the test module:

```rust
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
        self.lock().insert(name.to_string(), SecretString::from(value.to_string()));
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
```

  Check `thiserror` is already a dependency of `mur-agent-runtime` (`grep thiserror mur-agent-runtime/Cargo.toml`); it is used by `a2a_server.rs`'s `HandlerError`, so it is.
- [ ] **1.4 Watch it pass.** `cargo nextest run -p mur-agent-runtime secrets::` → `9 tests run: 9 passed`.
- [ ] **1.5 Commit.** `git branch --show-current` (separate call) → `feat/murmur-secret-handoff`. Then `git add mur-agent-runtime/src/secrets.rs mur-agent-runtime/src/lib.rs && git commit -m "feat(runtime): SecretVault — per-process store with env pairs, value masking, prompt fragment"`.

---

## Task 2 — `profile.secrets` names list

**Interfaces — Produces:** `mur_common::AgentProfile.secrets: Vec<String>` (serde default, skipped when empty).

- [ ] **2.1 Add the field.** In `mur-common/src/agent.rs`, directly after the `disabled_mcp` field (line ~115):

```rust
    /// Names of per-agent secrets the user handed this agent (murmur
    /// `/secret`, `mur agent secret set`). NAMES ONLY — the values live in the
    /// keychain under `mur-agent/<name>/<NAME>`. The list exists because the
    /// keychain cannot be enumerated: the supervisor reads it pre-seal to know
    /// which accounts to load. Empty = nothing to load (back-compat).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
```

- [ ] **2.2 Fix the exhaustive literals.** Add `secrets: Vec::new(),` immediately after the `disabled_mcp: Vec::new(),` line in `mur-core/src/cmd/agent/lifecycle.rs` (line ~199) and `mur-core/src/cmd/agent_companion/connector.rs` (line ~650). `default_for_tests()` deserializes a YAML fixture, so `#[serde(default)]` covers it.
- [ ] **2.3 Round-trip test.** In `mur-common/src/agent.rs` tests module (find `mod tests` near the bottom; add inside it):

```rust
    #[test]
    fn secrets_names_round_trip_and_are_absent_when_empty() {
        let mut p = AgentProfile::default_for_tests();
        let yaml = serde_yaml_ng::to_string(&p).unwrap();
        assert!(!yaml.contains("secrets:"), "empty list must not be written: {yaml}");
        p.secrets = vec!["GITEA_TOKEN".into()];
        let yaml = serde_yaml_ng::to_string(&p).unwrap();
        let back: AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(back.secrets, vec!["GITEA_TOKEN".to_string()]);
    }
```

- [ ] **2.4 Verify.** `cargo nextest run -p mur-common secrets_names` → `1 passed`. Then `MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo check --workspace --all-targets` → no errors. Also `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml` (workspace-excluded crate; memory `gotcha_workspace_excluded_addonref_literals`) → no errors.
- [ ] **2.5 Commit.** Branch check, then `git commit -am "feat(common): profile.secrets — names of per-agent keychain secrets to load pre-seal"`.

---

## Task 3 — Bash tool env injection

**Interfaces — Consumes:** `crate::secrets::SecretVault` (Task 1).
**Produces:** `BashTool.secrets: Option<Arc<SecretVault>>`, `BashTool::with_secrets(Arc<SecretVault>) -> Self`.

- [ ] **3.1 Failing test.** In `mur-agent-runtime/src/tools/bash.rs` tests module, after `captures_stderr`:

```rust
    #[cfg(unix)]
    #[tokio::test]
    async fn vault_values_reach_the_child_environment() {
        let vault = std::sync::Arc::new(crate::secrets::SecretVault::new());
        vault.set("GITEA_TOKEN", "d8b04a3cc632a5c8026cf5a810d36e292c603f99").unwrap();
        let t = make_tool().with_secrets(vault);
        let out = t
            .execute(serde_json::json!({"command": "printf '%s' \"$GITEA_TOKEN\""}))
            .await
            .unwrap();
        // The TOOL returns the raw value; masking is the runner's job at the
        // chokepoint, not this tool's. This test pins that division.
        assert_eq!(out.text.trim(), "d8b04a3cc632a5c8026cf5a810d36e292c603f99");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn no_vault_means_no_extra_environment() {
        let t = make_tool();
        let out = t
            .execute(serde_json::json!({"command": "printf '%s' \"${GITEA_TOKEN:-unset}\""}))
            .await
            .unwrap();
        assert_eq!(out.text.trim(), "unset");
    }
```

- [ ] **3.2 Watch it fail.** `cargo nextest run -p mur-agent-runtime vault_values_reach` → compile error: no method `with_secrets`.
- [ ] **3.3 Implement.** In `BashTool` struct add after `write_grants`:

```rust
    /// Credentials the user handed the agent, exported into every child's
    /// environment. `None` (tests, embedded uses) exports nothing.
    pub secrets: Option<std::sync::Arc<crate::secrets::SecretVault>>,
```

  In `BashTool::new` add `secrets: None,`. After `with_write_grants` add:

```rust
    /// Attach the vault whose values become the child's environment.
    pub fn with_secrets(mut self, vault: std::sync::Arc<crate::secrets::SecretVault>) -> Self {
        self.secrets = Some(vault);
        self
    }
```

  Replace the spawn chain (`let child = Command::new("bash") … .spawn()`) with:

```rust
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(&command)
            .current_dir(&working_dir)
            .env("PATH", path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // Values leave the vault only here, straight into the child's
        // environment. The parent never holds them as plain strings past this
        // statement.
        if let Some(vault) = &self.secrets {
            cmd.envs(vault.env_pairs());
        }
        let child = cmd.spawn().map_err(|e| {
```

  (keep the existing `map_err` body unchanged). In `mur-agent-runtime/src/tools/registry.rs` add `secrets: None,` to both `BashTool { … }` test literals (lines ~142 and ~176).
- [ ] **3.4 Watch it pass.** `cargo nextest run -p mur-agent-runtime tools::bash::` → all passed, including the two new ones.
- [ ] **3.5 Commit.** Branch check, then `git commit -am "feat(runtime): bash tool exports SecretVault values into the child environment"`.

---

## Task 4 — Runner: prompt fragment + masking chokepoint

**Interfaces — Consumes:** `SecretVault` (Task 1).
**Produces:** `TaskRunner::with_secrets(Arc<SecretVault>) -> Self`; private `TaskRunner::masked(&self, String) -> String`.

- [ ] **4.1 Failing tests.** In `mur-agent-runtime/src/task_runner.rs` tests module, after `saved_memory_reaches_the_system_prompt`:

```rust
    #[test]
    fn secret_names_reach_the_system_prompt_and_values_do_not() {
        let vault = Arc::new(crate::secrets::SecretVault::new());
        vault.set("GITEA_TOKEN", "d8b04a3cc632a5c8026cf5a810d36e292c603f99").unwrap();
        let runner = TaskRunner::new_stub_echo()
            .with_system_prompt(Some("BASE PROMPT".into()))
            .with_secrets(vault);
        let (sys, _) = runner.assemble_system_prompt("hello", None, None);
        assert!(sys.contains("$GITEA_TOKEN"), "{sys}");
        assert!(!sys.contains("d8b04a3c"), "{sys}");
    }

    #[test]
    fn tool_output_is_masked_before_it_becomes_a_result() {
        let vault = Arc::new(crate::secrets::SecretVault::new());
        vault.set("GITEA_TOKEN", "d8b04a3cc632a5c8026cf5a810d36e292c603f99").unwrap();
        let runner = TaskRunner::new_stub_echo().with_secrets(vault);
        assert_eq!(
            runner.masked("got d8b04a3cc632a5c8026cf5a810d36e292c603f99 back".into()),
            "got [SECRET:GITEA_TOKEN] back"
        );
        // No vault: passthrough.
        let bare = TaskRunner::new_stub_echo();
        assert_eq!(bare.masked("x".into()), "x");
    }
```

- [ ] **4.2 Watch them fail.** `cargo nextest run -p mur-agent-runtime secret_names_reach` → compile error: no method `with_secrets`.
- [ ] **4.3 Implement.** In the `TaskRunner` struct, after the `tools_policy` field:

```rust
    /// Credentials the user handed the agent. Names go into the system prompt;
    /// values are masked out of every tool result. `None` = no vault (stubs).
    secrets: Option<Arc<crate::secrets::SecretVault>>,
```

  There is one field initializer (`tools_policy: vec![],` at line ~348 — the named constructors all delegate to it); add `secrets: None,` beside it. After `with_tools_policy` add:

```rust
    pub fn with_secrets(mut self, vault: Arc<crate::secrets::SecretVault>) -> Self {
        self.secrets = Some(vault);
        self
    }

    /// The one place tool output is scrubbed before it can reach the model.
    /// Both tool-execution sites call this; a third site must too.
    fn masked(&self, output: String) -> String {
        match &self.secrets {
            Some(v) => v.mask(&output).into_owned(),
            None => output,
        }
    }
```

  In `assemble_system_prompt`, directly after `base.push_str(OUTPUT_LOCATIONS_RULE);`:

```rust
        if let Some(frag) = self.secrets.as_ref().and_then(|v| v.prompt_fragment()) {
            base.push_str(&frag);
        }
```

  At **both** execute sites (`match tool.execute(call.input.clone()).await {` near lines 1585 and 1715), change the `Ok` arm from `Ok(out) => (out.text, out.status, false, out.images),` to `Ok(out) => (self.masked(out.text), out.status, false, out.images),`. Note the `Err` arm formats `e` — a `ToolError` cannot carry a vault value (it is produced before the child runs), so it is left as is.
- [ ] **4.4 Watch them pass.** `cargo nextest run -p mur-agent-runtime task_runner::` → all passed.
- [ ] **4.5 Commit.** Branch check, then `git commit -am "feat(runtime): secret names in the system prompt, values masked at the tool-result chokepoint"`.

---

## Task 5 — A2A `secret/set` and `secret/delete`

**Interfaces — Consumes:** `SecretVault` (Task 1).
**Produces:** `SecretSetHandler::new(Arc<SecretVault>)`, `SecretDeleteHandler::new(Arc<SecretVault>)` in `crate::protocol::methods::secret_set`. Wire format: `secret/set` params `{"name": "GITEA_TOKEN", "value": "…"}` → result `{"name": "GITEA_TOKEN", "names": ["GITEA_TOKEN"], "effective": "next-turn"}`; `secret/delete` params `{"name": "GITEA_TOKEN"}` → result `{"name": "GITEA_TOKEN", "removed": true, "names": [], "effective": "next-turn"}`.

- [ ] **5.1 Failing tests.** Create `mur-agent-runtime/src/protocol/methods/secret_set.rs`:

```rust
//! A2A methods: `secret/set` and `secret/delete` — hand the RUNNING agent a
//! credential, or take one back.
//!
//! The durable half (keychain + `profile.secrets`) is written by the CLI
//! process before it dials this; this is how the sealed runtime learns about
//! it without a restart. Modelled on `memory/reload`: state changes on disk,
//! then the running agent is told.
//!
//! Nothing here logs, echoes, or returns the value. The response carries the
//! name and the resulting name list only — the CLI prints from that.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};
use crate::secrets::SecretVault;

pub struct SecretSetHandler {
    vault: Arc<SecretVault>,
}

pub struct SecretDeleteHandler {
    vault: Arc<SecretVault>,
}

impl SecretSetHandler {
    pub fn new(vault: Arc<SecretVault>) -> Self {
        Self { vault }
    }
}

impl SecretDeleteHandler {
    pub fn new(vault: Arc<SecretVault>) -> Self {
        Self { vault }
    }
}

fn str_param<'a>(params: &'a Option<Value>, key: &str) -> Result<&'a str, HandlerError> {
    params
        .as_ref()
        .and_then(|p| p.get(key))
        .and_then(Value::as_str)
        .ok_or_else(|| HandlerError::InvalidParams(format!("missing string param '{key}'")))
}

#[async_trait]
impl MethodHandler for SecretSetHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let name = str_param(&params, "name")?;
        let value = str_param(&params, "value")?;
        self.vault
            .set(name, value)
            .map_err(|e| HandlerError::InvalidParams(e.to_string()))?;
        tracing::info!(name, "secret/set: vault updated");
        Ok(json!({
            "name": name,
            "names": self.vault.names(),
            "effective": "next-turn",
        }))
    }
}

#[async_trait]
impl MethodHandler for SecretDeleteHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let name = str_param(&params, "name")?;
        let removed = self.vault.remove(name);
        tracing::info!(name, removed, "secret/delete");
        Ok(json!({
            "name": name,
            "removed": removed,
            "names": self.vault.names(),
            "effective": "next-turn",
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> RequestContext {
        RequestContext::default()
    }

    #[tokio::test]
    async fn set_then_delete_round_trips_and_never_returns_the_value() {
        let vault = Arc::new(SecretVault::new());
        let set = SecretSetHandler::new(vault.clone());
        let res = set
            .handle(Some(json!({"name": "GITEA_TOKEN", "value": "d8b04a3cc632a5c8026cf5a810d36e292c603f99"})), &ctx())
            .await
            .unwrap();
        assert_eq!(res["name"], "GITEA_TOKEN");
        assert_eq!(res["names"], json!(["GITEA_TOKEN"]));
        assert!(!res.to_string().contains("d8b04a3c"), "{res}");

        let del = SecretDeleteHandler::new(vault.clone());
        let res = del.handle(Some(json!({"name": "GITEA_TOKEN"})), &ctx()).await.unwrap();
        assert_eq!(res["removed"], true);
        assert_eq!(res["names"], json!([]));
        let res = del.handle(Some(json!({"name": "GITEA_TOKEN"})), &ctx()).await.unwrap();
        assert_eq!(res["removed"], false);
    }

    #[tokio::test]
    async fn a_short_value_is_rejected_as_invalid_params() {
        let set = SecretSetHandler::new(Arc::new(SecretVault::new()));
        let err = set
            .handle(Some(json!({"name": "K", "value": "short"})), &ctx())
            .await
            .unwrap_err();
        assert!(matches!(err, HandlerError::InvalidParams(_)), "{err:?}");
    }

    #[tokio::test]
    async fn missing_params_are_invalid_params_not_a_panic() {
        let set = SecretSetHandler::new(Arc::new(SecretVault::new()));
        assert!(matches!(set.handle(None, &ctx()).await.unwrap_err(), HandlerError::InvalidParams(_)));
        assert!(matches!(
            set.handle(Some(json!({"name": "K"})), &ctx()).await.unwrap_err(),
            HandlerError::InvalidParams(_)
        ));
    }
}
```

  (`RequestContext` derives `Default` — `a2a_server.rs:15`.)

  Add `pub mod secret_set;` to `mur-agent-runtime/src/protocol/methods/mod.rs` (alphabetical: after `pub mod model_set;`).
- [ ] **5.2 Watch them fail.** `cargo nextest run -p mur-agent-runtime secret_set::` → compile error or failures (handlers exist but nothing registered yet — tests are self-contained, so they should pass once the file compiles; if they pass immediately, that is the expected outcome for pure handler tests).
- [ ] **5.3 Register.** In `mur-agent-runtime/src/supervisor.rs` `build_dispatcher`: add a parameter `secrets: Arc<crate::secrets::SecretVault>,` after `runtime_skills`, and after the `memory/reload` registration:

```rust
    // murmur `/secret`. The CLI has already written the keychain and
    // `profile.secrets`; this is how the sealed runtime learns without a
    // restart. Unconditional: every agent shape can carry an env var.
    d.register(
        "secret/set",
        Box::new(crate::protocol::methods::secret_set::SecretSetHandler::new(secrets.clone())),
    );
    d.register(
        "secret/delete",
        Box::new(crate::protocol::methods::secret_set::SecretDeleteHandler::new(secrets)),
    );
```

  The call site (line ~522) gets `secrets.clone(),` as the new last argument — `secrets` is created in Task 6; for this step create it right before the call as `let secrets = Arc::new(crate::secrets::SecretVault::new());` so the crate compiles, and move it in Task 6.
- [ ] **5.4 Verify.** `cargo nextest run -p mur-agent-runtime secret_set::` → `3 passed`; `cargo check -p mur-agent-runtime --all-targets` clean.
- [ ] **5.5 Commit.** Branch check, then `git add -A mur-agent-runtime && git commit -m "feat(runtime): secret/set and secret/delete A2A methods"`.

---

## Task 6 — Supervisor wiring: pre-seal load, vault threading

**Interfaces — Consumes:** `SecretVault`, `KEYCHAIN_SERVICE` (Task 1); `profile.secrets` (Task 2); `BashTool::with_secrets` (Task 3); `TaskRunner::with_secrets` (Task 4); dispatcher param (Task 5).
**Produces:** `prepare_runtime(agent_home, profile, socket_enabled, secrets: Arc<SecretVault>)`; `build_runner(…, secrets: Option<Arc<SecretVault>>)` as its new last parameter.

- [ ] **6.1 Pre-seal load.** In `supervisor.rs`, replace the temporary `let secrets = …` from 5.3 and add, inside the pre-seal block right after the `for key in ["ANTHROPIC_API_KEY", "OPENAI_API_KEY"] { … }` loop (still inside the `{ … }` that computes `cached`):

```rust
        // User-handed secrets (murmur `/secret`, `mur agent secret set`). Names
        // come from the profile because the keychain cannot be enumerated;
        // values are resolved here, pre-seal, for the same reason as the
        // provider keys above. A value the CLI could write but the vault will
        // not hold (too short) is skipped with a warning — the CLI enforces
        // the same floor, so this only fires for hand-written keychain items.
        for name in &profile.inner.secrets {
            let r = mur_common::secret::SecretRef::Keychain {
                service: crate::secrets::KEYCHAIN_SERVICE.to_string(),
                account: format!("{}/{}", profile.inner.name, name),
            };
            match mur_common::secret::cache_before_seal(&r) {
                Ok(()) => {
                    if let Some(v) = r.resolve_preseal_cached() {
                        use secrecy::ExposeSecret;
                        match secrets.set(name, v.expose_secret()) {
                            Ok(()) => cached += 1,
                            Err(e) => warn!(name, error = %e, "secret skipped"),
                        }
                    }
                }
                Err(e) => warn!(
                    name,
                    error = %e,
                    "could not resolve a user secret before sealing; \
                     the agent will not have it until it is set again"
                ),
            }
        }
```

  and, **above** the whole pre-seal block (before `let reg = …`), `let secrets = Arc::new(crate::secrets::SecretVault::new());`. Pass `secrets.clone()` to `prepare_runtime(&agent_home, &profile, socket_enabled, secrets.clone())` (line ~476) and keep `secrets.clone()` as the last argument to `build_dispatcher`.
- [ ] **6.2 Thread through `supervisor_runner.rs`.** Add `secrets: Arc<crate::secrets::SecretVault>` as the last parameter of `prepare_runtime` (line ~631) and of `build_provider_runner` (line ~214, if `prepare_runtime` delegates the bash-tool construction there — follow where `BashTool::new` at line ~288 lives and give *that* function the parameter). At the `BashTool::new(…)` chain add `.with_secrets(secrets.clone())` after `.with_write_grants(…)`. Add `secrets: Option<Arc<crate::secrets::SecretVault>>` as the last parameter of `build_runner` and inside it `if let Some(v) = secrets { runner = runner.with_secrets(v); }` next to the existing `if let Some(n) = max_iterations` block. At the `build_runner(` call inside the `build` closure (line ~455) pass `Some(secrets.clone())`; at `task_runner.rs:3854` (test) pass `None`.
- [ ] **6.3 Verify.** `cargo check -p mur-agent-runtime --all-targets` clean; `cargo nextest run -p mur-agent-runtime` → all passed (full crate: this touched shared signatures).
- [ ] **6.4 Commit.** Branch check, then `git commit -am "feat(runtime): load profile.secrets from the keychain pre-seal and thread the vault to bash, runner, and dispatcher"`.

---

## Task 7 — TUI parsing: `SlashCmd::Secret`

**Interfaces — Produces:** `SlashCmd::Secret { key: Option<String>, delete: bool }`; `App.pending_secret_prompt: Option<String>` (the key awaiting hidden input).

- [ ] **7.1 Failing test.** In `mur-core/src/cmd/agent/cli/app.rs` tests, after `parse_slash_login`:

```rust
    #[test]
    fn parse_slash_secret() {
        let s = |key: Option<&str>, delete| Some(SlashCmd::Secret {
            key: key.map(str::to_string),
            delete,
        });
        assert_eq!(parse_slash("/secret"), s(None, false));
        assert_eq!(parse_slash("/secret GITEA_TOKEN"), s(Some("GITEA_TOKEN"), false));
        assert_eq!(parse_slash("/secret GITEA_TOKEN --delete"), s(Some("GITEA_TOKEN"), true));
        assert_eq!(parse_slash("/secret --delete GITEA_TOKEN"), s(Some("GITEA_TOKEN"), true));
        // Only the KEY is parsed here; a value on the same line is never
        // accepted, so nothing typed after the key can be a secret in history.
        assert_eq!(parse_slash("/secret GITEA_TOKEN somevalue"), s(Some("GITEA_TOKEN"), false));
    }
```

- [ ] **7.2 Watch it fail.** `MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core parse_slash_secret` → compile error: no variant `Secret`.
- [ ] **7.3 Implement.** In `SlashCmd` after the `Login` variant:

```rust
    /// `/secret <KEY> [--delete]` — hand the agent a credential through a
    /// hidden prompt, or revoke one. Only the KEY is on this line by design.
    Secret {
        key: Option<String>,
        delete: bool,
    },
```

  In `parse_slash`, after the `"login"` arm:

```rust
        "secret" => {
            let args: Vec<&str> = words.collect();
            SlashCmd::Secret {
                key: args
                    .iter()
                    .find(|s| !s.starts_with("--"))
                    .map(|s| (*s).to_string()),
                delete: args.contains(&"--delete"),
            }
        }
```

  In `App` after `pending_handover`: `/// A `/secret KEY` waiting for the main loop to read the value with the terminal handed over. \n pub pending_secret_prompt: Option<String>,` and `pending_secret_prompt: None,` in the constructor beside `pending_handover: None,`.

  In `mur-core/src/cmd/agent/cli/mod.rs`: `help_name` gets `SlashCmd::Secret { .. } => Some("secret"),`; `one_of_each` gets `SlashCmd::Secret { key: None, delete: false },`; `HELP` gets `  /secret <KEY> [--delete] (hand the agent a credential — hidden input, never enters the chat)` inserted after the `/login …)` clause. In `complete.rs` `COMMANDS` add `("secret", "hand the agent a credential (hidden input)", &["--delete"]),` between `("quit", …)` and `("sessions", …)` (the list is alphabetical). The dispatch `match` in `mod.rs` will fail to compile until Task 8 adds the arm — add a temporary arm `SlashCmd::Secret { .. } => app.push_system("secret: not wired yet".into()),` now and replace it in Task 8.
- [ ] **7.4 Watch it pass.** Same command → `parse_slash_secret` passed; also run `help_lists_every_command` → passed.
- [ ] **7.5 Commit.** Branch check, then `git commit -am "feat(murmur): parse /secret <KEY> [--delete]"`.

---

## Task 8 — Hidden read + `/secret` handler + reporting

**Interfaces — Consumes:** `SlashCmd::Secret`, `App.pending_secret_prompt` (Task 7); `dial_method` (`crate::a2a_dial`); `mur_common::secret::{keychain_set, keychain_delete}`; `crate::cmd::agent::{load_profile_for_edit, save_profile}`; `crate::cmd::agent::secret::SECRET_SERVICE`.
**Produces:** `handover::read_hidden(terminal, viewport_h, prompt) -> Result<String>`; `secret_cmd::{request, delete, StepReport, report_line, after_hidden_input}`.

- [ ] **8.1 Failing tests for the pure parts.** Create `mur-core/src/cmd/agent/cli/secret_cmd.rs` with the test module only:

```rust
//! murmur `/secret` — hand the running agent a credential without the value
//! ever touching the chat.
//!
//! Two writes, reported separately (spec §6): the keychain + `profile.secrets`
//! make it durable across restarts; `secret/set` over the unix socket makes
//! it live in the sealed runtime NOW. A keychain failure stops before the
//! dial. Keychain ✓ with the agent unreachable prints two distinct lines —
//! collapsing them into one ✓ is exactly how a user ends up pasting the
//! token into the chat "because the agent said it didn't have it".

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_both_ok_is_one_line() {
        assert_eq!(
            report_line("GITEA_TOKEN", &StepReport::Both),
            "✓ GITEA_TOKEN available as $GITEA_TOKEN"
        );
    }

    #[test]
    fn report_agent_not_reached_is_two_facts_not_one_tick() {
        let line = report_line("GITEA_TOKEN", &StepReport::DurableOnly("connection refused".into()));
        assert!(line.starts_with("saved to keychain ✓"), "{line}");
        assert!(line.contains("running agent: not reached"), "{line}");
        assert!(line.contains("restart to load"), "{line}");
        assert!(!line.contains("available as"), "{line}");
    }

    #[test]
    fn a_method_not_found_is_reported_as_an_older_runtime() {
        let line = report_line(
            "K",
            &StepReport::DurableOnly(r#"{"code":-32601,"message":"method not found"}"#.into()),
        );
        assert!(line.contains("predates /secret"), "{line}");
        assert!(!line.contains("-32601"), "{line}");
    }

    #[test]
    fn validation_happens_before_any_write() {
        assert_eq!(validate("gitea", "d8b04a3cc632a5c8026cf5a810d36e292c603f99").unwrap_err(), "secret name 'gitea' must match [A-Z_][A-Z0-9_]*");
        assert_eq!(validate("K", "short").unwrap_err(), "value must be at least 8 characters (got 5)");
        assert_eq!(validate("K", "").unwrap_err(), "cancelled (empty value)");
        assert!(validate("GITEA_TOKEN", "d8b04a3cc632a5c8026cf5a810d36e292c603f99").is_ok());
    }
}
```

- [ ] **8.2 Watch it fail.** `… cargo nextest run -p mur-core secret_cmd::` → compile error.
- [ ] **8.3 Implement `secret_cmd.rs`** (above the tests):

```rust
use anyhow::{Context, Result};
use std::path::Path;

use super::app::App;
use crate::a2a_dial::{DialMode, dial_method};
use crate::cmd::agent::secret::SECRET_SERVICE;
use crate::cmd::agent::{load_profile_for_edit, save_profile};

/// Same floor as `mur_agent_runtime::secrets::MIN_LEN`. Duplicated on
/// purpose: `mur-core` must not depend on the runtime crate, and the runtime
/// re-checks on its side anyway.
pub const MIN_LEN: usize = 8;

/// `[A-Z_][A-Z0-9_]*` — mirrors `mur_agent_runtime::secrets::valid_name`.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

pub fn validate(name: &str, value: &str) -> std::result::Result<(), String> {
    if !valid_name(name) {
        return Err(format!("secret name '{name}' must match [A-Z_][A-Z0-9_]*"));
    }
    if value.is_empty() {
        return Err("cancelled (empty value)".into());
    }
    let n = value.chars().count();
    if n < MIN_LEN {
        return Err(format!("value must be at least {MIN_LEN} characters (got {n})"));
    }
    Ok(())
}

/// What happened, step by step. `Both` is the only state that earns a ✓ line.
pub enum StepReport {
    Both,
    /// Keychain + profile written; the dial failed with this message.
    DurableOnly(String),
}

pub fn report_line(name: &str, r: &StepReport) -> String {
    match r {
        StepReport::Both => format!("✓ {name} available as ${name}"),
        StepReport::DurableOnly(e) if e.contains("-32601") => format!(
            "saved to keychain ✓ · running agent: not reached — its runtime predates /secret \
             (mur update, then restart to load)"
        ),
        StepReport::DurableOnly(e) => {
            format!("saved to keychain ✓ · running agent: not reached ({e}) — restart to load")
        }
    }
}

/// `/secret KEY` — validate the name now, then ask the main loop to read the
/// value with the terminal handed over (the composer must never see it).
pub fn request(app: &mut App, key: Option<String>, delete: bool) {
    let Some(key) = key else {
        list(app);
        return;
    };
    if !valid_name(&key) {
        app.push_error(format!("secret name '{key}' must match [A-Z_][A-Z0-9_]*"));
        return;
    }
    if delete {
        app.push_system(format!("{key}: removing…"));
        app.pending_secret_delete = Some(key);
        return;
    }
    app.pending_secret_prompt = Some(key);
}

fn list(app: &mut App) {
    match load_profile_for_edit(&app.agent) {
        Ok((_, p)) if p.secrets.is_empty() => {
            app.push_system("no secrets set — /secret <KEY> to hand the agent one".into())
        }
        Ok((_, p)) => app.push_system(format!(
            "secrets (names only): {}\n/secret <KEY> to add or replace · /secret <KEY> --delete to revoke",
            p.secrets.join(", ")
        )),
        Err(e) => app.push_error(format!("read profile: {e:#}")),
    }
}

/// Step ①: keychain, then `profile.secrets`. Either failure aborts.
async fn write_durable(home: &Path, agent: &str, name: &str, value: &str) -> Result<()> {
    let _ = home; // profile path is resolved through the same helper the CLI uses
    let acct = format!("{agent}/{name}");
    mur_common::secret::keychain_set(SECRET_SERVICE, &acct, value)
        .await
        .with_context(|| format!("keychain write {SECRET_SERVICE}/{acct}"))?;
    let (path, mut profile) = load_profile_for_edit(agent)?;
    if !profile.secrets.iter().any(|n| n == name) {
        profile.secrets.push(name.to_string());
        save_profile(&path, &mut profile).context("profile write (secrets list)")?;
    }
    Ok(())
}

async fn remove_durable(agent: &str, name: &str) -> Result<()> {
    let acct = format!("{agent}/{name}");
    mur_common::secret::keychain_delete(SECRET_SERVICE, &acct)
        .await
        .with_context(|| format!("keychain delete {SECRET_SERVICE}/{acct}"))?;
    let (path, mut profile) = load_profile_for_edit(agent)?;
    let before = profile.secrets.len();
    profile.secrets.retain(|n| n != name);
    if profile.secrets.len() != before {
        save_profile(&path, &mut profile).context("profile write (secrets list)")?;
    }
    Ok(())
}

/// Step ②: tell the running agent. Any error becomes `DurableOnly`.
async fn dial(app: &App, method: &'static str, params: serde_json::Value) -> StepReport {
    let (h, ag) = (app.home.clone(), app.agent.clone());
    match tokio::task::spawn_blocking(move || dial_method(&h, &ag, method, params, DialMode::Auto))
        .await
    {
        Ok(Ok(_)) => StepReport::Both,
        Ok(Err(e)) => StepReport::DurableOnly(e.to_string()),
        Err(e) => StepReport::DurableOnly(format!("dial task failed: {e}")),
    }
}

/// Called by the main loop with the hidden-read value. Validates, writes,
/// dials, reports. The value is dropped at the end of this function.
pub async fn after_hidden_input(app: &mut App, name: String, value: String) {
    if let Err(e) = validate(&name, &value) {
        app.push_error(format!("{name}: {e}"));
        return;
    }
    if let Err(e) = write_durable(&app.home, &app.agent, &name, &value).await {
        app.push_error(format!("{name}: {e:#} — nothing sent to the agent"));
        return;
    }
    let r = dial(app, "secret/set", serde_json::json!({ "name": name, "value": value })).await;
    app.push_system(report_line(&name, &r));
}

pub async fn after_delete(app: &mut App, name: String) {
    if let Err(e) = remove_durable(&app.agent, &name).await {
        app.push_error(format!("{name}: {e:#}"));
        return;
    }
    let r = dial(app, "secret/delete", serde_json::json!({ "name": name })).await;
    app.push_system(match r {
        StepReport::Both => format!("✓ {name} removed"),
        StepReport::DurableOnly(e) => {
            format!("removed from keychain ✓ · running agent: not reached ({e}) — it keeps the value until restart")
        }
    });
}
```

  Add `pub mod secret_cmd;` to `mur-core/src/cmd/agent/cli/mod.rs` beside the other `mod` lines, and `pub pending_secret_delete: Option<String>,` (+ `None` init) to `App` next to `pending_secret_prompt`. `load_profile_for_edit`/`save_profile` are `pub(crate)` in `cmd/agent/mod.rs`; `SECRET_SERVICE` is `pub(crate)` in `cmd/agent/secret.rs` — both reachable.
- [ ] **8.4 Hidden read.** In `mur-core/src/cmd/agent/cli/handover.rs`, after `pub fn run`:

```rust
/// Read one line with echo off, with the terminal handed over exactly as
/// `run` does for a child. Same contract: the caller has dropped the
/// `EventStream` and recreates it afterwards. Returns the line without its
/// newline; an empty line is the user backing out.
pub fn read_hidden(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    viewport_h: u16,
    prompt: &str,
) -> Result<String> {
    let line = {
        let _suspended = Suspended::begin(viewport_h)?;
        let mut out = io::stdout();
        write!(out, "{prompt}")?;
        out.flush()?;
        // rpassword handles echo-off on the tty it opens itself, so this does
        // not depend on the raw-mode state `Suspended` just released.
        rpassword::read_password().context("read hidden value")?
    };
    reanchor(terminal, viewport_h)?;
    Ok(line)
}
```

- [ ] **8.5 Main loop.** In `mur-core/src/cmd/agent/cli/mod.rs`, directly after the `pending_handover` block (the one ending with `app.needs_full_redraw = true; }`), add:

```rust
        if app.render_mode == RenderMode::Inline
            && let Some(key) = app.pending_secret_prompt.take()
        {
            let want_h = prepare_handover(app, &format!("/secret {key}"), last_size.height);
            drop(events);
            if want_h != viewport_h && handover::reanchor(terminal, want_h).is_ok() {
                viewport_h = want_h;
            }
            ui::flush_finished(terminal, app, viewport_h)?;
            terminal.draw(|f| ui::render(f, app))?;
            let read = handover::read_hidden(
                terminal,
                viewport_h,
                &format!("Enter value for {key} (input hidden, Enter alone cancels): "),
            );
            events = EventStream::new();
            match read {
                Ok(value) => secret_cmd::after_hidden_input(app, key, value).await,
                Err(e) => app.push_error(format!("{key}: hidden read failed: {e:#}")),
            }
            app.needs_full_redraw = true;
        }
        if let Some(key) = app.pending_secret_delete.take() {
            secret_cmd::after_delete(app, key).await;
        }
```

  Replace the temporary dispatch arm from 7.3 with `SlashCmd::Secret { key, delete } => secret_cmd::request(app, key, delete),`.

  `/secret` in Fullscreen render mode: `pending_secret_prompt` is only consumed in `Inline` mode (same as `pending_handover`); in `secret_cmd.rs` change the import to `use super::app::{App, RenderMode};` and add to `request()` before setting the field: `if app.render_mode != RenderMode::Inline { app.push_error("/secret needs the inline view — close the overlay (Esc) and try again"); return; }` (`RenderMode` already derives `PartialEq, Eq` — `app.rs:335`).
- [ ] **8.6 Verify.** `MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core cli::` → all passed (the four `secret_cmd` tests plus existing). `cargo clippy --workspace --all-targets -- -D warnings` clean (read the exit code, not the grep — memory `gotcha_local_clippy_without_d_warnings_lies`). `cargo fmt --all`.
- [ ] **8.7 Commit.** Branch check, then `git add -A && git commit -m "feat(murmur): /secret — hidden input, keychain + profile, secret/set dial, per-step reporting"`.

---

## Task 9 — Real-machine verification + docs

**Interfaces — Consumes:** everything above, installed.

- [ ] **9.1 Install the branch build.** `./build.sh --install` (→ `~/.local/bin`; confirm with `readlink -f $(command -v murmur)` that it is not the brew copy — memory `gotcha_brew_vs_buildsh_install_collision`). Restart one test agent so it runs the new runtime: `mur agent restart <agent>`.
- [ ] **9.2 Live run.** `murmur <agent>`, type `/secret GITEA_TOKEN`, enter a 40-char dummy value `d8b04a3cc632a5c8026cf5a810d36e292c603f99`. Expected TUI line: `✓ GITEA_TOKEN available as $GITEA_TOKEN`. Then ask the agent: `print the length of $GITEA_TOKEN and then the value` — expected: the tool card shows `40` and `[SECRET:GITEA_TOKEN]`, never the hex.
- [ ] **9.3 Leak check.** In a shell: `command grep -rl d8b04a3cc632a5c8026cf5a810d36e292c603f99 ~/.mur/agents/<agent>/ ~/.mur/queue/ 2>/dev/null` → **no output**. `mur agent secret list <agent>` → lists the model secret; `grep secrets: ~/.mur/agents/<agent>/profile.yaml` → `secrets:` with `- GITEA_TOKEN`. (Negative control, per memory `gotcha_test_passed_while_proving_nothing`: run the same grep against a file you deliberately wrote the value into, confirm it finds it, delete that file.)
- [ ] **9.4 Restart path.** `mur agent restart <agent>`, reopen `murmur <agent>`, ask again for the length → `40` without re-entering. Then `/secret GITEA_TOKEN --delete` → `✓ GITEA_TOKEN removed`; ask for the length → `0`.
- [ ] **9.5 Stale-runtime path.** Stop the agent (`mur agent stop <agent>`), run `/secret GITEA_TOKEN` again → line begins `saved to keychain ✓ · running agent: not reached`. Start it again; confirm the value is present (length `40`). Clean up: `/secret GITEA_TOKEN --delete`.
- [ ] **9.6 Docs.** `CLAUDE.md` CLI-surface line for `mur agent`: after the `/model [N|name]` clause add `, and `/secret <KEY> [--delete]` (hand the running agent a credential through a hidden prompt: keychain + `profile.secrets` for restarts, `secret/set` A2A for the live process; the model only ever sees `$KEY`, and every tool result is masked — see `docs/superpowers/specs/2026-09-07-murmur-secret-handoff-design.md`)`. Then invoke the `update-docs` skill for README, the docs site, and the product page.
- [ ] **9.7 Commit + PR.** Branch check; `git commit -am "docs: /secret in CLAUDE.md, README, docs site"`; push; mark #1203 ready for review with the 9.2–9.5 observations pasted into the PR body as the verification section.

---

## Self-review

- **Spec coverage:** §Decision 1 (model sees names) → Tasks 1, 4. §2 dual-write → Task 8 (`write_durable` + `dial`). §3 system prompt names → Tasks 1, 4. §4 persistent per-agent, `--delete` → Tasks 2, 6, 8. §5 masking chokepoint ≥ 8 + ceiling test → Tasks 1 (`mask_does_not_catch_a_reencoded_value`), 4. §6 per-step reporting → Task 8 (`report_line` tests). Error table: keychain failure aborts → `after_hidden_input`; `-32601` → `report_line`; invalid KEY/short → `validate`; delete split-report → `after_delete`. Verification section → Task 9.
- **Placeholders:** none. Every symbol referenced was verified against the tree at `4adedb10` on 2026-09-07/08.
- **Cross-task consistency:** `with_secrets` is the builder name on both `BashTool` (Task 3) and `TaskRunner` (Task 4); `SecretVault::{set, remove, names, env_pairs, mask, prompt_fragment}` used in Tasks 3–5, 8 match Task 1's signatures; `KEYCHAIN_SERVICE` (runtime) and `SECRET_SERVICE` (core) are both `"mur-agent"` and the plan says so; `profile.secrets` is read in Task 6 and written in Task 8 under the same name; `MIN_LEN` is 8 in both crates with the duplication justified.
