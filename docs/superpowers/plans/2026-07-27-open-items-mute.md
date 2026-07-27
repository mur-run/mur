# Open Items Mute Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user permanently collapse a noisy `origin` out of `mur open`, without ever hiding the fact that something is collapsed.

**Architecture:** Mute state is a list of exact `origin` strings in `config.yaml`. `collect()` is untouched and still returns everything; a new `partition()` applies the policy above it, so collectors stay ignorant of display rules and `--json` can report both halves. Writing that config safely requires fixing a pre-existing data-loss bug first, which is Task 1.

**Tech Stack:** Rust 2024, `serde_yaml` (note: `mur-core/src/store/config.rs` uses `serde_yaml`, not `serde_yaml_ng`), `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-07-27-open-items-mute-design.md`

## Global Constraints

- Rust edition 2024 — `let` chains are stable and used in this codebase.
- Build/test env: `export ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist` and put `~/.rustup/toolchains/stable-aarch64-apple-darwin/bin` on `PATH`, or `mur-core` will not link.
- Use `cargo nextest run`, not `cargo test` — plain `cargo test` fails ~7 tests spuriously in this repo from `MUR_HOME` cross-test interference.
- Lint gate is `cargo clippy --lib --bins -- -D warnings`. `--tests` is already red repo-wide and is not your diff.
- Mute matching is **exact string equality** on `origin`, never prefix. `fleet` must not match `fleet:acme`.
- On any error reading config, the mute set is empty and everything shows. Fail toward showing, never toward hiding.
- Single source file ≤ 800 lines; `mur-core/src/open_items/mod.rs` is ~213 lines and has room.

---

### Task 1: Make config writes lossless

`save_config_at` serialises the typed `Config` and writes the result, so any top-level block `Config` has no field for is silently deleted. Verified on a real config: `research_gateway: { brave_api_key_ref: keychain:mur/brave }` disappears. Ten-plus call sites do this today (`mur sleep`, `mur model add`, `mur source`, `mur team`, `mur init`, and `/skin` in the agent CLI). Mute cannot add an eleventh.

Fixes #778.

**Files:**
- Modify: `mur-core/src/store/config.rs` — `save_config_at`, plus a new private helper
- Test: `mur-core/src/store/config.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing
- Produces: `save_config_at(&Path, &Config) -> Result<()>` — same signature, now non-destructive. Task 5 relies on this.

- [ ] **Step 1: Write the failing test**

Append to (or create) the `#[cfg(test)] mod tests` block at the end of `mur-core/src/store/config.rs`:

```rust
#[cfg(test)]
mod save_roundtrip_tests {
    use super::*;
    use mur_common::config::Config;

    /// `Config` cannot round-trip a block it has no field for, so serialising
    /// the struct alone deletes it. Measured on a real config: a hand-written
    /// `research_gateway` block vanished on the next `mur sleep`.
    #[test]
    fn save_preserves_blocks_the_typed_config_does_not_know() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.yaml");
        std::fs::write(
            &path,
            "research_gateway:\n  brave_api_key_ref: keychain:mur/brave\n",
        )
        .unwrap();

        let cfg = Config::load_or_default(&path);
        save_config_at(&path, &cfg).unwrap();

        let back = std::fs::read_to_string(&path).unwrap();
        assert!(back.contains("research_gateway"), "block dropped:\n{back}");
        assert!(back.contains("keychain:mur/brave"), "value dropped:\n{back}");
    }

    /// The typed fields must still win — an unknown-block merge that also
    /// resurrected stale known values would be a different bug.
    #[test]
    fn typed_fields_still_overwrite_the_old_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.yaml");
        std::fs::write(&path, "session:\n  retention_days: 3\nkeep_me: yes\n").unwrap();

        let mut cfg = Config::load_or_default(&path);
        cfg.session.retention_days = 99;
        save_config_at(&path, &cfg).unwrap();

        let back = std::fs::read_to_string(&path).unwrap();
        assert!(back.contains("99"), "typed field not written:\n{back}");
        assert!(back.contains("keep_me"), "unknown key dropped:\n{back}");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
export ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
cargo nextest run -p mur-core --lib -E 'test(save_roundtrip_tests)'
```

Expected: `save_preserves_blocks_the_typed_config_does_not_know` FAILS with `block dropped:` and no `research_gateway` in the output. The second test passes already.

- [ ] **Step 3: Implement the merge**

In `mur-core/src/store/config.rs`, replace the serialise line inside `save_config_at`:

```rust
    // Serialize to YAML, then prepend a header with model recommendations
    let yaml = serde_yaml::to_string(config)?;
```

with:

```rust
    // Serialize to YAML, then prepend a header with model recommendations
    let yaml = merge_over_existing(path, config)?;
```

and add this private helper immediately above `save_config_at`:

```rust
/// Serialise `config`, then restore any top-level block the typed `Config`
/// has no field for.
///
/// `Config` cannot round-trip what it cannot parse, so serialising the struct
/// alone silently deletes user blocks — `research_gateway` was measurably lost
/// on every `mur sleep` (#778). Typed fields win; only keys absent from the
/// new document are carried over from the old one.
fn merge_over_existing(path: &Path, config: &Config) -> Result<String> {
    let mut out = serde_yaml::to_value(config)?;
    let existing: Option<serde_yaml::Value> = fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_yaml::from_str(&s).ok());

    if let (Some(serde_yaml::Value::Mapping(old)), serde_yaml::Value::Mapping(new)) =
        (existing, &mut out)
    {
        for (k, v) in old {
            if !new.contains_key(&k) {
                new.insert(k, v);
            }
        }
    }
    Ok(serde_yaml::to_string(&out)?)
}
```

If `use std::fs;` and `use std::path::Path;` are not already in scope at the top of the file, add them.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo nextest run -p mur-core --lib -E 'test(save_roundtrip_tests)'
```

Expected: 2 passed.

- [ ] **Step 5: Verify against the real config, non-destructively**

```bash
cp ~/.mur/config.yaml /tmp/config-before.yaml
python3 -c "
import yaml
a=yaml.safe_load(open('/tmp/config-before.yaml')) or {}
print('top-level keys:', len(a))
print('research_gateway present:', 'research_gateway' in a)
"
```

Expected: `top-level keys: 25`, `research_gateway present: True`. Do not run a command that writes the real config; the unit tests cover the behaviour.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/store/config.rs
git commit -m "fix(config): stop deleting config blocks the typed struct does not know

save_config_at serialised the typed Config and wrote the result, so any
top-level block Config has no field for was silently dropped. Measured on a
real config: research_gateway (the deep-research Brave key binding) vanished.
Ten-plus call sites do this today — mur sleep, mur model add, mur source, mur
team, mur init, and /skin in the agent CLI.

The write now merges over the existing document: typed fields win, unknown
keys pass through. Closes #778."
```

---

### Task 2: Mute config + partition

**Files:**
- Modify: `mur-common/src/config.rs` — add `OpenItemsConfig`, add the field to `Config`
- Modify: `mur-core/src/open_items/mod.rs` — add `partition`, make `fingerprint` operate on the visible set
- Test: both files, inline

**Interfaces:**
- Consumes: nothing from Task 1 at compile time
- Produces:
  - `mur_common::config::OpenItemsConfig { pub muted: Vec<String> }`, reachable as `cfg.open_items.muted`
  - `mur_core::open_items::partition(items: Vec<OpenItem>, muted: &[String]) -> (Vec<OpenItem>, Vec<String>)` — returns `(visible, muted_origins_that_matched)`, second element sorted and deduplicated. Tasks 3, 4 and 5 all call this.

- [ ] **Step 1: Write the failing tests**

In `mur-common/src/config.rs`, inside the existing `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn open_items_muted_parses_and_defaults_empty() {
        let c: Config = serde_yaml::from_str("open_items:\n  muted:\n    - inbox\n").unwrap();
        assert_eq!(c.open_items.muted, vec!["inbox".to_string()]);

        let d: Config = serde_yaml::from_str("llm:\n  model: x\n").unwrap();
        assert!(d.open_items.muted.is_empty(), "must default to no mutes");
    }

    /// Fail toward showing. A config that will not parse must yield an empty
    /// mute set, never a quiet, confident, incomplete list.
    #[test]
    fn unreadable_config_yields_no_mutes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.yaml");
        std::fs::write(&path, "this: is: not: valid: yaml: [[[\n").unwrap();
        let cfg = Config::load_or_default(&path);
        assert!(
            cfg.open_items.muted.is_empty(),
            "a broken config must hide nothing"
        );

        // Same for a config that is simply absent.
        let missing = Config::load_or_default(&tmp.path().join("nope.yaml"));
        assert!(missing.open_items.muted.is_empty());
    }
```

In `mur-core/src/open_items/mod.rs`, inside the existing `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn partition_hides_muted_origins_and_names_them() {
        let items = vec![
            OpenItem { origin: "inbox".into(), ..item(ItemSource::Observed, "a", Utc::now()) },
            OpenItem { origin: "fleet:x".into(), ..item(ItemSource::Observed, "b", Utc::now()) },
        ];
        let (visible, muted) = partition(items, &["inbox".to_string()]);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].origin, "fleet:x");
        assert_eq!(muted, vec!["inbox".to_string()]);
    }

    /// `fleet` must not swallow `fleet:acme`. Prefix matching is the one
    /// outcome a mute must never produce by accident.
    #[test]
    fn mute_matching_is_exact_not_prefix() {
        let items = vec![OpenItem {
            origin: "fleet:acme".into(),
            ..item(ItemSource::Observed, "a", Utc::now())
        }];
        let (visible, muted) = partition(items, &["fleet".to_string()]);
        assert_eq!(visible.len(), 1, "prefix must not match");
        assert!(muted.is_empty());
    }

    /// A configured mute that matched nothing is not named — the footer
    /// reports what the reader would otherwise have seen, not the config.
    #[test]
    fn a_mute_that_matched_nothing_is_not_reported() {
        let items = vec![OpenItem {
            origin: "inbox".into(),
            ..item(ItemSource::Observed, "a", Utc::now())
        }];
        let (_, muted) = partition(items, &["fleet:gone".to_string()]);
        assert!(muted.is_empty());
    }

    /// Muting a noisy source has to silence the turn notice too, or the mute
    /// does nothing where it matters most.
    #[test]
    fn fingerprint_over_visible_ignores_muted_churn() {
        let mk = |n: usize| OpenItem {
            title: format!("{n} proposals"),
            origin: "inbox".into(),
            ..item(ItemSource::Observed, "x", Utc::now())
        };
        let (v1, _) = partition(vec![mk(246)], &["inbox".to_string()]);
        let (v2, _) = partition(vec![mk(300)], &["inbox".to_string()]);
        assert_eq!(fingerprint(&v1), fingerprint(&v2));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo nextest run -p mur-common -p mur-core --lib -E 'test(open_items) or test(partition) or test(mute_matching)'
```

Expected: compile error — `partition` not found, `open_items` field not found on `Config`.

- [ ] **Step 3: Add the config struct**

In `mur-common/src/config.rs`, next to `FleetRunConfig` (around line 217):

```rust
/// Display policy for `mur open`.
///
/// Lives in `config.yaml` rather than in `open-items.jsonl` because that log
/// is append-only and agent-writable via the `open_item` tool. A user's
/// decision to stop looking at a source must not be overturnable by an agent
/// appending a record. Same reasoning as `fleet_run.agents`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OpenItemsConfig {
    /// Exact `origin` strings to collapse out of `mur open`. Exact match
    /// only — `fleet` never matches `fleet:acme`.
    #[serde(default)]
    pub muted: Vec<String>,
}
```

And on `Config`, immediately after the `fleet_run` field (around line 189):

```rust
    // --- `mur open` display policy ---
    #[serde(default)]
    pub open_items: OpenItemsConfig,
```

If `Config` is constructed with exhaustive struct literals anywhere, add `open_items: Default::default()`. Find them with:

```bash
grep -rn "Config {" --include="*.rs" . | grep -v "^./target" | grep -v "\.\.Default::default()"
```

- [ ] **Step 4: Implement partition and re-point fingerprint**

In `mur-core/src/open_items/mod.rs`, add after `collect`:

```rust
/// Split `items` by the mute list.
///
/// Returns the items to show, and the muted origins that actually matched
/// something — the footer names what the reader would otherwise have seen,
/// not what the config happens to contain, so a stale mute stays quiet.
pub fn partition(items: Vec<OpenItem>, muted: &[String]) -> (Vec<OpenItem>, Vec<String>) {
    let mut hidden: Vec<String> = Vec::new();
    let visible: Vec<OpenItem> = items
        .into_iter()
        .filter(|it| {
            // Exact match, never prefix: `fleet` must not swallow `fleet:acme`.
            if muted.iter().any(|m| m == &it.origin) {
                if !hidden.contains(&it.origin) {
                    hidden.push(it.origin.clone());
                }
                false
            } else {
                true
            }
        })
        .collect();
    hidden.sort();
    (visible, hidden)
}
```

`fingerprint` needs no change — callers now pass it the visible slice.

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo nextest run -p mur-common -p mur-core --lib -E 'test(open_items) or test(partition) or test(mute_matching)'
```

Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/config.rs mur-core/src/open_items/mod.rs
git commit -m "feat(open-items): mute config and partition

Muted origins live in config.yaml, not the open-items log — that log is
agent-writable, and a user's decision to stop looking must not be
overturnable by an agent appending a record.

Matching is exact. Prefix matching would let 'fleet' silently swallow every
fleet in the list, which is the one thing a mute must never do by accident."
```

---

### Task 3: The footer

Muting is only safe because it collapses rather than hides. This task is that guarantee.

**Files:**
- Modify: `mur-core/src/open_items/mod.rs` — `render`
- Test: same file, inline

**Interfaces:**
- Consumes: `partition` from Task 2
- Produces: `render(items: &[OpenItem], muted: &[String]) -> String` — **signature changes**, gaining the second parameter. Task 5 calls it.

- [ ] **Step 1: Write the failing tests**

```rust
    /// A permanent mute is only safe if the list always says something is
    /// muted. The reader must never have to wonder whether anything is hidden.
    #[test]
    fn footer_names_muted_sources() {
        let out = render(
            &[item(ItemSource::Observed, "visible", Utc::now())],
            &["inbox".to_string(), "fleet:old".to_string()],
        );
        assert!(out.contains("2 sources muted"), "{out}");
        assert!(out.contains("inbox"), "{out}");
        assert!(out.contains("fleet:old"), "{out}");
        assert!(out.contains("mur open --all"), "{out}");
    }

    #[test]
    fn no_footer_when_nothing_is_muted() {
        let out = render(&[item(ItemSource::Observed, "a", Utc::now())], &[]);
        assert!(!out.contains("muted"), "{out}");
    }

    /// Everything muted is not the same as nothing outstanding, and the
    /// difference has to be visible.
    #[test]
    fn everything_muted_still_shows_the_footer() {
        let out = render(&[], &["inbox".to_string()]);
        assert!(out.contains("1 source muted"), "{out}");
        assert!(out.contains("No open items"), "{out}");
    }
```

- [ ] **Step 2: Run to verify they fail**

```bash
cargo nextest run -p mur-core --lib -E 'test(footer) or test(muted)'
```

Expected: compile error — `render` takes 1 argument.

- [ ] **Step 3: Implement**

Change the signature and the empty-case early return in `mur-core/src/open_items/mod.rs`:

```rust
pub fn render(items: &[OpenItem], muted: &[String]) -> String {
    let mut out = if items.is_empty() {
        "No open items.\n".to_string()
    } else {
        let mut s = String::new();
        let mut last: Option<ItemSource> = None;
        for it in items {
            if last != Some(it.source) {
                let caveat = match it.source {
                    ItemSource::Observed => "from MUR's own state",
                    ItemSource::Reported => "an agent said so, unverified",
                };
                s.push_str(&format!(
                    "\n{} {} — {caveat}\n",
                    it.source.marker(),
                    it.source.label()
                ));
                last = Some(it.source);
            }
            s.push_str(&format!("  {} [{}]\n", it.title, it.origin));
            if let Some(next) = &it.next {
                s.push_str(&format!("      → {next}\n"));
            }
        }
        s
    };

    // Collapsed, never hidden. This one line is what makes a permanent mute
    // safe: the reader never has to wonder whether something is missing.
    if !muted.is_empty() {
        out.push_str(&format!(
            "\n{} source{} muted ({}) — mur open --all\n",
            muted.len(),
            if muted.len() == 1 { "" } else { "s" },
            muted.join(", ")
        ));
    }
    out
}
```

- [ ] **Step 4: Run to verify they pass**

```bash
cargo nextest run -p mur-core --lib -E 'test(open_items)'
```

Expected: all pass, including the pre-existing `empty_says_so_rather_than_printing_a_bare_header` (update its call to `render(&[], &[])`).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/open_items/mod.rs
git commit -m "feat(open-items): footer naming muted sources

Mute collapses, it never hides. The trade is one line versus N, not show
versus hide — which is what makes a mute that never expires safe to have."
```

---

### Task 4: CLI surface

**Files:**
- Modify: `mur-core/src/cli/mod.rs` — add `--all` to `Commands::Open`
- Modify: `mur-core/src/cli/actions.rs` — add `Mute` / `Unmute` to `OpenAction`
- Modify: `mur-core/src/dispatch.rs` — the `Commands::Open` arm
- Test: `mur-core/src/dispatch.rs` is not unit-tested; verification is the manual run in Step 4

**Interfaces:**
- Consumes: `partition` (Task 2), `render(items, muted)` (Task 3), `save_config_at` (Task 1)
- Produces: no new library symbols

- [ ] **Step 1: Add the flag and the subcommands**

In `mur-core/src/cli/mod.rs`, the `Open` variant gains `all`:

```rust
    /// Show what is still outstanding, labelled by whether MUR observed it or
    /// an agent merely reported it
    Open {
        #[command(subcommand)]
        action: Option<OpenAction>,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
        /// Include muted sources
        #[arg(long)]
        all: bool,
    },
```

In `mur-core/src/cli/actions.rs`, extend `OpenAction`:

```rust
    /// Stop showing a source. Exact `origin` match — `fleet` never matches
    /// `fleet:acme`.
    Mute {
        /// Origin as shown in brackets, e.g. `inbox` or `fleet:acme`
        origin: String,
    },
    /// Show a muted source again
    Unmute {
        origin: String,
    },
```

- [ ] **Step 2: Wire the dispatch arm**

Replace the whole `Commands::Open { .. }` arm in `mur-core/src/dispatch.rs`:

```rust
        Commands::Open { action, json, all } => {
            let home = crate::paths::mur_root(None);
            let cfg_path = home.join("config.yaml");
            match action {
                Some(OpenAction::Add { title, agent, next }) => {
                    let id =
                        crate::open_items::reported::report(&home, &agent, &title, next.as_deref())?;
                    println!("Recorded (reported by {agent}): {id}");
                }
                Some(OpenAction::Done { id }) => {
                    crate::open_items::reported::resolve(&home, &id)?;
                    println!("Resolved {id}");
                }
                Some(OpenAction::Mute { origin }) => {
                    let mut cfg = mur_common::config::Config::load_or_default(&cfg_path);
                    // Record even when nothing currently carries it — a source
                    // can be legitimately empty today — but say so, so a typo
                    // surfaces here rather than as a silent no-op later.
                    let seen: Vec<String> = crate::open_items::collect(&home)
                        .into_iter()
                        .map(|i| i.origin)
                        .collect();
                    if !seen.contains(&origin) {
                        let mut uniq: Vec<String> = seen;
                        uniq.sort();
                        uniq.dedup();
                        eprintln!(
                            "warning: nothing currently has origin '{origin}' (in use: {})",
                            if uniq.is_empty() { "none".into() } else { uniq.join(", ") }
                        );
                    }
                    if !cfg.open_items.muted.contains(&origin) {
                        cfg.open_items.muted.push(origin.clone());
                        cfg.open_items.muted.sort();
                        crate::store::config::save_config_at(&cfg_path, &cfg)?;
                    }
                    println!("Muted {origin}");
                }
                Some(OpenAction::Unmute { origin }) => {
                    let mut cfg = mur_common::config::Config::load_or_default(&cfg_path);
                    cfg.open_items.muted.retain(|m| m != &origin);
                    crate::store::config::save_config_at(&cfg_path, &cfg)?;
                    // Unmuting something that was not muted is not an error:
                    // the requested end state holds either way.
                    println!("Unmuted {origin}");
                }
                None => {
                    let items = crate::open_items::collect(&home);
                    // Fail toward showing: an unreadable config yields no
                    // mutes, never a quiet, confident, incomplete list.
                    let muted_cfg = if all {
                        Vec::new()
                    } else {
                        mur_common::config::Config::load_or_default(&cfg_path)
                            .open_items
                            .muted
                    };
                    let (visible, muted) = crate::open_items::partition(items, &muted_cfg);
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "items": visible,
                                "muted": muted,
                            }))?
                        );
                    } else {
                        print!("{}", crate::open_items::render(&visible, &muted));
                    }
                }
            }
        }
```

Add `OpenAction` to the `use crate::cli::{...}` import list at the top of `dispatch.rs` if it is not already there.

- [ ] **Step 3: Build**

```bash
cargo build -q -p mur-core --bin mur
```

Expected: no errors.

- [ ] **Step 4: Verify by hand against a scratch MUR_HOME**

Do not test against the real `~/.mur`.

```bash
export MUR_HOME=/tmp/mur-mute-test
rm -rf "$MUR_HOME" && mkdir -p "$MUR_HOME/fleets/demo"
touch "$MUR_HOME/fleets/demo/.stopped"
mkdir -p "$MUR_HOME/inbox/workflow-proposals" && touch "$MUR_HOME/inbox/workflow-proposals/a.yaml"

./target/debug/mur open                 # two observed lines, no footer
./target/debug/mur open mute inbox      # "Muted inbox"
./target/debug/mur open                 # one line + "1 source muted (inbox)"
./target/debug/mur open --all           # two lines, no footer
./target/debug/mur open --json          # {"items":[...],"muted":["inbox"]}
./target/debug/mur open mute typo       # warns, names origins in use
./target/debug/mur open unmute inbox    # back to two lines
grep -A2 open_items "$MUR_HOME/config.yaml"
unset MUR_HOME
```

Expected: exactly as annotated. `open_items.muted` present in the scratch config after the mute, gone after the unmute.

- [ ] **Step 5: Lint and commit**

```bash
cargo clippy -p mur-core --lib --bins -- -D warnings
git add mur-core/src/cli/mod.rs mur-core/src/cli/actions.rs mur-core/src/dispatch.rs
git commit -m "feat(open-items): mur open mute/unmute and --all

Muting an origin that nothing currently carries is allowed — a source can be
legitimately empty today — but warns and names the origins in use, so a typo
surfaces at the point of the mistake instead of as a silent no-op.

--json changes shape from a bare array to {items, muted}. It shipped hours
ago with no known consumer, so the break is taken now rather than carried
forever as a second output mode; without it Hub cannot tell an empty list
from a fully muted one."
```

---

### Task 5: The turn summary respects mutes

Muting a noisy source has to silence the end-of-turn line too. That line is where the noise is most expensive, because it interrupts.

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/app.rs` — `note_open_items_if_changed`
- Test: same file, inline

**Interfaces:**
- Consumes: `partition` (Task 2)
- Produces: nothing new

- [ ] **Step 1: Write the failing test**

In the `#[cfg(test)] mod session_cost_tests` block or a new `mod open_items_tests` in `app.rs`:

```rust
#[cfg(test)]
mod open_items_notice_tests {
    use super::*;

    /// A muted source must not wake the turn notice, or the mute does nothing
    /// where it costs the most — the line that interrupts.
    #[test]
    fn muted_source_does_not_change_the_fingerprint() {
        use crate::open_items::{ItemSource, OpenItem, fingerprint, partition};
        let mk = |n: usize| OpenItem {
            title: format!("{n} proposals awaiting review"),
            next: None,
            source: ItemSource::Observed,
            origin: "inbox".into(),
            at: chrono::Utc::now(),
        };
        let muted = vec!["inbox".to_string()];
        let (a, _) = partition(vec![mk(246)], &muted);
        let (b, _) = partition(vec![mk(999)], &muted);
        assert_eq!(fingerprint(&a), fingerprint(&b));
    }
}
```

- [ ] **Step 2: Run it**

```bash
cargo nextest run -p mur-core --lib -E 'test(open_items_notice_tests)'
```

Expected: PASS.

**This test passes before Step 3 and that is not a mistake — but it does mean the test alone cannot tell you the wiring landed.** It pins the property (`partition` then `fingerprint` is blind to muted churn) so a later refactor cannot quietly break it. Whether `note_open_items_if_changed` actually *calls* that pair is verified in Step 4 against a real temp home, because the method reads config and the filesystem and reaches the user only through `push_system`. Do not skip Step 4 on the strength of a green Step 2.

- [ ] **Step 3: Wire the mute into the notice**

Replace `note_open_items_if_changed` in `mur-core/src/cmd/agent/cli/app.rs`:

```rust
    /// After a turn, say what is outstanding — but only if the set changed.
    ///
    /// Repeating the same three items after every turn is how a status line
    /// becomes wallpaper. Staying silent when nothing moved is what keeps the
    /// line worth reading on the turn something does. Muted sources are
    /// removed before the comparison, so muting silences this too.
    pub fn note_open_items_if_changed(&mut self) {
        let muted = mur_common::config::Config::load_or_default(&self.home.join("config.yaml"))
            .open_items
            .muted;
        let (visible, _) = crate::open_items::partition(crate::open_items::collect(&self.home), &muted);
        let fp = crate::open_items::fingerprint(&visible);
        if self.open_items_fp == Some(fp) {
            return;
        }
        self.open_items_fp = Some(fp);
        if let Some(line) = crate::open_items::summary_line(&visible) {
            self.push_system(line);
        }
    }
```

And in `mur-core/src/cmd/agent/cli/mod.rs`, the `SlashCmd::Open` arm — `/open` shows everything, because asking for the list explicitly is not the same as being interrupted by it:

```rust
        SlashCmd::Open => {
            let items = crate::open_items::collect(&app.home);
            let muted = mur_common::config::Config::load_or_default(&app.home.join("config.yaml"))
                .open_items
                .muted;
            let (visible, muted_names) = crate::open_items::partition(items, &muted);
            app.open_items_fp = Some(crate::open_items::fingerprint(&visible));
            app.push_system(
                crate::open_items::render(&visible, &muted_names)
                    .trim()
                    .to_string(),
            );
        }
```

- [ ] **Step 4: Run the full affected suite**

```bash
cargo nextest run -p mur-common -p mur-core --lib \
  -E 'test(open_items) or test(agent::cli) or test(save_roundtrip)'
cargo clippy -p mur-common -p mur-core --lib --bins -- -D warnings
```

Expected: all pass; clippy clean.

Then verify the wiring Step 2 could not — that the turn notice actually goes quiet. Against a scratch home, not the real one:

```bash
export MUR_HOME=/tmp/mur-notice-test
rm -rf "$MUR_HOME" && mkdir -p "$MUR_HOME/inbox/workflow-proposals"
touch "$MUR_HOME/inbox/workflow-proposals/"{a,b,c}.yaml

./target/debug/mur open                    # "3 harvested workflow proposals"
./target/debug/mur open mute inbox
touch "$MUR_HOME/inbox/workflow-proposals/"{d,e}.yaml   # the muted source changes
./target/debug/mur open                    # still only the footer, no count line
./target/debug/mur open --all              # "5 harvested workflow proposals"
unset MUR_HOME
```

Expected: after the mute, adding proposals changes nothing visible except under `--all`. If the count line reappears, `note_open_items_if_changed` is not calling `partition` and Step 3 did not land.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/cli/app.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(open-items): turn notice and /open respect mutes

The end-of-turn line is where noise costs most, because it interrupts — so
the fingerprint is computed over the visible set and a muted source can no
longer wake it. /open still renders the footer, because asking for the list
is not the same as being interrupted by it."
```

---

## Verification before opening the PR

```bash
export ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
RUST_MIN_STACK=33554432 cargo nextest run -p mur-common -p mur-core --lib \
  -E 'test(open_items) or test(config) or test(agent::cli)'
cargo clippy -p mur-common -p mur-core -p mur-agent-runtime --lib --bins -- -D warnings
cargo fmt --all --check
```

Then confirm the real config is still intact — this is the bug Task 1 fixes, and the whole point is that it stays fixed:

```bash
python3 -c "
import yaml
a=yaml.safe_load(open('$HOME/.mur/config.yaml')) or {}
print('top-level keys:', len(a), '| research_gateway:', 'research_gateway' in a)
"
```

Expected: `top-level keys: 25 | research_gateway: True`.

## Out of scope

Deliberately not in this plan, per the spec:

- Snooze / time-boxed mute. Mute is permanent until reversed.
- Per-item mute. `mur open done` clears a reported item; observed items clear themselves.
- Agent-side injection of open items into session-start context — its own spec.
- Fixing the harvest proposal generator (#777).
