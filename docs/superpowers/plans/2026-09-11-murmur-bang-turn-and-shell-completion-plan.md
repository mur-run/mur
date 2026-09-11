# Plan: murmur `!cmd` output as a turn; shell completion in the composer

> Execute with **`mur-executing-plans`** (in-context, task by task). Spec:
> `docs/superpowers/specs/2026-09-11-murmur-bang-turn-and-shell-completion-design.md`.
> Base: `main` **after** #1255 (menu marks current) and #1256 (help_text)
> have merged — Task 3 edits `refresh_completion` / `completion_accept` /
> `help_text`, which exist only after those two.

**Goal.** A `!cmd` run in murmur sends its output to the agent as the user's
turn immediately, and `!` lines get a live completion menu (commands, then
paths).

**Architecture.** All in `mur-core/src/cmd/agent/cli/`. Task 1 reroutes the
`StreamMsg::ShellDone` event: instead of stashing the block for the next
typed message it starts a turn (idle), steers (streaming) or notes (over
budget). Task 2 adds a pure `shell_complete` module. Task 3 wires that
module into the existing menu (`refresh_completion` dispatches on the first
character) and updates `/help`.

**Tech stack.** Rust 2024, ratatui TUI, `cargo nextest`. Build/test env for
`mur-core`: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432`.

## Global Constraints (from the spec — every task includes these)

- Scope: `mur-core/src/cmd/agent/cli/` only. No runtime or protocol change.
- The output is a turn: `!cmd` finishes → the block is sent to the agent as the user's message, immediately. No "run only, don't send" variant.
- No stash survives anywhere: `pending_shell`, `take_pending_shell` and their test are deleted; typed messages carry no shell prefix.
- The Shell card is the transcript entry: a shell turn must not push a `Role::User` message and must persist exactly one channel event (the existing `"shell"` one).
- Completion: last word only; first word = PATH command names (scanned once, lazily); later words = cwd-relative paths with `~`; hidden entries only when the prefix starts with `.`; directories first, trailing `/`, `has_children = true`; cap `complete::MAX_MENU_ROWS`; no escaping of spaces (named ceiling).
- Before every commit: `cargo fmt -p mur-core`, `cargo clippy -p mur-core --all-targets -- -D warnings` (with the env above) clean, the named tests green.

## File structure

| File | Responsibility | Task |
|---|---|---|
| `mur-core/src/cmd/agent/cli/app.rs` | `begin_turn` split out of `begin_user_turn`; `pending_shell` + `take_pending_shell` + their test removed; `path_bins` cache | 1, 3 |
| `mur-core/src/cmd/agent/cli/mod.rs` | `shell_block`, `ShellRoute` + `route_shell_output`, `start_shell_turn`, `steer_now` (extracted from `submit`), `ShellDone` handler, `refresh_completion` dispatch, `completion_accept`, `help_text` row, tests | 1, 3 |
| `mur-core/src/cmd/agent/cli/shell_complete.rs` (new) | pure `candidates()` for `!` lines + `scan_path_bins()` + unit tests | 2 |

---

## Task 1 — `!cmd` output is a turn

**Interfaces.**
- Consumes: `App::push_shell(&mut self, cmd: &str, output: &str)` (exists), `stream::steer_turn(home, agent, task_id, msg)` (exists), `build_params(text, task_id, context_task_id, image, cwd)` (exists), `spawn_stream(home, agent, params, task_id, tx)` (exists).
- Produces: `App::begin_turn(&mut self) -> String` (task id; no bubble, no persist); `fn shell_block(cmd: &str, output: &str) -> String`; `enum ShellRoute { Start, Steer(String), Skip(&'static str) }`; `fn route_shell_output(streaming: bool, task_id: Option<&str>, over_budget: bool) -> ShellRoute`; `fn start_shell_turn(app, block, tx)`; `fn steer_now(app, task_id, msg, label, tx)`.

### Steps

- [x] **1.1 Write the failing routing test** — append to the `#[cfg(test)] mod hitl_key_tests` block in `mod.rs` (the module that already has `image_does_not_ride_a_steer_and_stays_staged`), a new module right after it:

```rust
#[cfg(test)]
mod shell_turn_tests {
    use super::*;

    #[test]
    fn shell_output_routes_by_turn_state() {
        assert!(matches!(route_shell_output(false, None, false), ShellRoute::Start));
        assert!(matches!(route_shell_output(true, Some("t1"), false), ShellRoute::Steer(ref t) if t == "t1"));
        assert!(matches!(route_shell_output(true, None, false), ShellRoute::Skip(_)));
        // Budget gates a NEW turn only; a steer rides the turn already paid for.
        assert!(matches!(route_shell_output(false, None, true), ShellRoute::Skip(_)));
        assert!(matches!(route_shell_output(true, Some("t1"), true), ShellRoute::Steer(_)));
    }

    #[test]
    fn shell_block_frames_command_and_output() {
        assert_eq!(
            shell_block("ls", "a\nb"),
            "[shell command the user ran locally]\n$ ls\na\nb\n[end of shell output]"
        );
        assert_eq!(
            shell_block("true", ""),
            "[shell command the user ran locally]\n$ true\n[end of shell output]"
        );
    }

    /// Idle: the block becomes the outgoing user message, the transcript keeps
    /// the one Shell card and gains no User bubble.
    #[tokio::test]
    async fn shell_done_while_idle_starts_a_turn_without_a_user_bubble() {
        let (tx, _rx) = mpsc::channel(16);
        let mut app = App::test_fixture();
        handle_stream(&mut app, StreamMsg::ShellDone { cmd: "ls".into(), output: "a\nb".into() }, &tx);
        assert_eq!(app.messages.iter().filter(|m| m.role == Role::Shell).count(), 1);
        assert_eq!(app.messages.iter().filter(|m| m.role == Role::User).count(), 0);
        assert!(app.streaming, "a turn started");
        let params = app.inflight_params.clone().expect("params kept for replay");
        let text = params["message"]["parts"][0]["text"].as_str().unwrap();
        assert!(text.contains("$ ls\na\nb"), "{text}");
        assert!(text.starts_with("[shell command the user ran locally]"), "{text}");
    }

    /// Streaming: the block steers the live turn; no second turn starts.
    #[tokio::test]
    async fn shell_done_while_streaming_steers_the_live_turn() {
        let (tx, _rx) = mpsc::channel(16);
        let mut app = App::test_fixture();
        let before = app.begin_user_turn("working");
        handle_stream(&mut app, StreamMsg::ShellDone { cmd: "ls".into(), output: "a".into() }, &tx);
        assert_eq!(app.current_task_id.as_deref(), Some(before.as_str()), "same turn");
        assert!(app.messages.iter().any(|m| m.text.contains("↗ steering: $ ls output")), "{:?}", app.messages.iter().map(|m| m.text.clone()).collect::<Vec<_>>());
        assert_eq!(app.messages.iter().filter(|m| m.role == Role::Shell).count(), 1);
    }
}
```

- [x] **1.2 Watch it fail** — `cargo nextest run -p mur-core --lib -E 'test(shell_turn_tests)'` (with the env). Expected: compile error `cannot find function route_shell_output` (and `shell_block`, `ShellRoute`).

- [x] **1.3 Split `begin_turn` out of `begin_user_turn`** in `app.rs` — replace the whole `begin_user_turn` body with:

```rust
    pub fn begin_user_turn(&mut self, text: &str) -> String {
        self.messages.push(ChatMsg::new(Role::User, text));
        self.persist_turn("user", text, None, &[]);
        self.begin_turn()
    }

    /// Start a turn the transcript already shows — a `!cmd` whose Shell card
    /// is its entry. Everything `begin_user_turn` does except the User bubble
    /// and the `"user"` channel event.
    pub fn begin_turn(&mut self) -> String {
        // A fresh client-side task id per turn (used for cancellation).
        let task_id = uuid::Uuid::now_v7().to_string();
        self.current_task_id = Some(task_id.clone());
        self.inflight_params = None;
        self.send_retried = false;
        self.turn_produced_output = false;
        self.streaming = true;
        self.turn_started = Some(std::time::Instant::now());
        self.turn_in = 0;
        self.turn_out = 0;
        self.saw_step_this_turn = false;
        self.saw_hitl_this_turn = false;
        self.pending_suggestions.clear();
        self.scroll_back = 0;
        // Placeholder agent message that deltas accumulate into.
        let mut m = ChatMsg::new(Role::Agent, "");
        m.streaming = true;
        self.messages.push(m);
        task_id
    }
```

- [x] **1.4 Delete the stash** in `app.rs`: remove the field `pub pending_shell: Vec<String>,` (with its doc comment), its initialiser `pending_shell: Vec::new(),`, the line `self.pending_shell.push(text);` inside `push_shell`, the whole `take_pending_shell` fn (and its doc comment), and the test `shell_blocks_queue_and_drain_into_prefix`. `push_shell` becomes:

```rust
    /// Record a completed `!command` run: show it and persist it. The block is
    /// sent to the agent by `handle_stream`'s `ShellDone` arm, not stashed.
    pub fn push_shell(&mut self, cmd: &str, output: &str) {
        let text = if output.is_empty() {
            format!("$ {cmd}")
        } else {
            format!("$ {cmd}\n{output}")
        };
        self.messages.push(ChatMsg::new(Role::Shell, text.clone()));
        self.persist_turn("shell", &text, None, &[]);
        self.scroll_back = 0;
    }
```

- [x] **1.5 `start_turn` no longer prefixes** — in `mod.rs` replace the body of `fn start_turn` with:

```rust
fn start_turn(app: &mut App, trimmed: String, tx: &mpsc::Sender<StreamMsg>) {
    let task_id = app.begin_user_turn(&trimmed);
    // The working directory is NOT prose here — it rides as `context.cwd`
    // in `build_params`, every turn.
    let params = build_params(
        &trimmed,
        &task_id,
        app.context_task_id.as_deref(),
        app.pending_image
            .as_ref()
            .map(|(m, b)| (m.as_str(), b.as_str())),
        app.cwd.as_deref(),
    );
    app.pending_image = None;
    app.inflight_params = Some(params.clone());
    spawn_stream(
        app.home.clone(),
        app.agent.clone(),
        params,
        task_id,
        tx.clone(),
    );
}
```

- [x] **1.6 Add the shell-turn helpers** directly below `start_turn` in `mod.rs`:

```rust
/// What the agent receives for a `!cmd` run. Singular, framed, nothing else:
/// the agent may answer with one line, and the block does not ask for more.
fn shell_block(cmd: &str, output: &str) -> String {
    if output.is_empty() {
        format!("[shell command the user ran locally]\n$ {cmd}\n[end of shell output]")
    } else {
        format!("[shell command the user ran locally]\n$ {cmd}\n{output}\n[end of shell output]")
    }
}

/// Where a finished `!cmd` block goes.
#[derive(Debug)]
enum ShellRoute {
    /// Idle: start a turn with the block as the user's message.
    Start,
    /// A turn is live: steer it with the block.
    Steer(String),
    /// Nowhere; the note says why. The Shell card still renders.
    Skip(&'static str),
}

/// Pure so the three routes are testable without pricing or a live agent.
/// The budget gates a NEW turn only, exactly as `submit` does for typed text:
/// a steer rides the turn already being paid for.
fn route_shell_output(streaming: bool, task_id: Option<&str>, over_budget: bool) -> ShellRoute {
    if streaming {
        return match task_id {
            Some(t) => ShellRoute::Steer(t.to_string()),
            None => ShellRoute::Skip("shell output not sent — a turn is generating without a task id"),
        };
    }
    if over_budget {
        return ShellRoute::Skip("↯ shell output not sent — session budget reached");
    }
    ShellRoute::Start
}

/// Start a turn whose transcript entry is the Shell card already pushed by
/// `push_shell`: no User bubble, no second channel event. A staged image is
/// left staged — it belongs to the user's next typed message.
fn start_shell_turn(app: &mut App, block: String, tx: &mpsc::Sender<StreamMsg>) {
    let task_id = app.begin_turn();
    let params = build_params(&block, &task_id, app.context_task_id.as_deref(), None, app.cwd.as_deref());
    app.inflight_params = Some(params.clone());
    spawn_stream(app.home.clone(), app.agent.clone(), params, task_id, tx.clone());
}

/// Inject `msg` into the live turn `task_id`. `label` is what the transcript
/// shows after "↗ steering:" — the typed text for a message, a short tag for
/// a shell block that would otherwise fill the screen twice.
fn steer_now(app: &mut App, task_id: String, msg: String, label: &str, tx: &mpsc::Sender<StreamMsg>) {
    let (h, a) = (app.home.clone(), app.agent.clone());
    let t = tx.clone();
    app.push_system(format!("↗ steering: {label}"));
    tokio::spawn(async move {
        if let Err(e) = stream::steer_turn(h, a, task_id.clone(), msg.clone()).await {
            let err = format!("{e:#}");
            let out = match recover::classify_steer_failure(&err) {
                // The runtime restarted (tasks live in memory only):
                // the steered task is gone. Drop the dead binding and
                // replay the text as a fresh turn on the same channel so
                // it is not lost (#713).
                recover::SteerFailure::TaskGone => StreamMsg::TurnLost {
                    task_id,
                    note: "agent restarted — continuing in this conversation".to_string(),
                    resend: Some(msg),
                },
                recover::SteerFailure::Other => StreamMsg::Note(format!("steer failed: {err}")),
            };
            let _ = t.send(out).await;
        }
    });
}
```

- [x] **1.7 Make `submit` use `steer_now`** — in `submit`'s `if app.streaming {` branch, replace everything from `if let Some(task_id) = app.current_task_id.clone() {` through the matching `} else { app.push_system("still generating — press Ctrl+C to cancel first"); }` with:

```rust
        if let Some(task_id) = app.current_task_id.clone() {
            app.clear_input();
            steer_now(app, task_id, trimmed.clone(), &trimmed, tx);
        } else {
            app.push_system("still generating — press Ctrl+C to cancel first");
        }
```

  (The `↗ steering:` line now comes from `steer_now`; the old inline `tokio::spawn` block is gone.)

- [x] **1.8 Route `ShellDone`** — in `handle_stream`, replace `StreamMsg::ShellDone { cmd, output } => app.push_shell(&cmd, &output),` with:

```rust
        StreamMsg::ShellDone { cmd, output } => {
            app.push_shell(&cmd, &output);
            let block = shell_block(&cmd, &output);
            match route_shell_output(app.streaming, app.current_task_id.as_deref(), app.over_budget()) {
                ShellRoute::Start => start_shell_turn(app, block, tx),
                ShellRoute::Steer(task_id) => {
                    let label = format!("$ {cmd} output");
                    steer_now(app, task_id, block, &label, tx);
                }
                ShellRoute::Skip(why) => app.push_system(why),
            }
        }
```

  `handle_stream`'s early return drops events whose task id is not current; `ShellDone` has no task id (`msg.task_id()` is `None` for it), so it always reaches this arm — verify by reading `StreamMsg::task_id` in `stream.rs`; if `ShellDone` is not in its `None` list, add it there.

- [x] **1.9 Watch it pass** — `cargo nextest run -p mur-core --lib -E 'test(shell_turn_tests) | test(/cmd::agent::cli::/)'`. Expected: all pass, including the whole `cli` module (the deleted stash test is gone, nothing else referenced `take_pending_shell`). Then fmt + clippy per Global Constraints.

- [x] **1.10 Commit** — `git add mur-core/src/cmd/agent/cli && git commit` with message:

```
feat(murmur): a !cmd's output is the user's next turn

Winning shape: Claude Code's `!` mode. The block is sent as soon as the
command finishes — idle starts a turn, a live turn is steered, over budget
notes and keeps the Shell card. The next-message stash (`pending_shell`)
and the prefix it added to typed messages are gone.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 2 — `shell_complete.rs`: pure candidates for `!` lines

**Interfaces.**
- Consumes: `complete::Candidate { display, insert, desc, has_children }`, `complete::MAX_MENU_ROWS` (both exist).
- Produces: `pub struct ShellCompleteCtx<'a> { pub cwd: &'a Path, pub path_bins: &'a [String], pub home: Option<&'a Path> }`; `pub fn candidates(line: &str, ctx: &ShellCompleteCtx<'_>) -> Vec<Candidate>`; `pub fn scan_path_bins() -> Vec<String>`.

### Steps

- [x] **2.1 Create the module with its tests first** — new file `mur-core/src/cmd/agent/cli/shell_complete.rs`:

```rust
//! Completion for `!` lines in the composer: command names for the first
//! word (from a PATH scan the caller owns and caches), cwd-relative paths for
//! every later word. Pure over what it is handed, so the tests run on a
//! tempdir and a literal command list.
//!
//! ponytail: inserted paths are not quoted. A name with a space goes in as-is
//! and the user quotes it; escaping is the upgrade if that ever bites.

use std::path::{Path, PathBuf};

use super::complete::{Candidate, MAX_MENU_ROWS};

pub struct ShellCompleteCtx<'a> {
    /// Directory relative paths resolve against (murmur's shell cwd).
    pub cwd: &'a Path,
    /// Executable names on `$PATH`, sorted, deduplicated.
    pub path_bins: &'a [String],
    /// What a leading `~` expands to. `None` leaves `~` alone.
    pub home: Option<&'a Path>,
}

/// Split the text after `!` into the head (kept verbatim, trailing space
/// included) and the last word (the one being completed).
fn split_last_word(body: &str) -> (&str, &str) {
    match body.rfind(' ') {
        Some(i) => (&body[..=i], &body[i + 1..]),
        None => ("", body),
    }
}

/// Candidates for the composer line `line`, which must start with `!`.
/// `insert` is the whole line with the last word replaced.
pub fn candidates(line: &str, ctx: &ShellCompleteCtx<'_>) -> Vec<Candidate> {
    if line.contains('\n') {
        return Vec::new();
    }
    let Some(body) = line.strip_prefix('!') else {
        return Vec::new();
    };
    let body = body.trim_start();
    let (head, word) = split_last_word(body);
    let mut out = if head.is_empty() {
        command_candidates(word, ctx.path_bins)
    } else {
        path_candidates(word, ctx)
    };
    out.truncate(MAX_MENU_ROWS);
    for c in &mut out {
        c.insert = format!("!{head}{}", c.insert);
    }
    out
}

/// First word: command names by prefix. An empty prefix offers nothing —
/// eight of two thousand commands is noise, not help.
fn command_candidates(word: &str, bins: &[String]) -> Vec<Candidate> {
    if word.is_empty() {
        return Vec::new();
    }
    bins.iter()
        .filter(|b| b.starts_with(word))
        .map(|b| Candidate {
            display: b.clone(),
            insert: format!("{b} "),
            desc: String::new(),
            has_children: false,
        })
        .collect()
}

/// A later word: entries of the word's directory, by file-name prefix.
/// Directories first with a trailing `/` and `has_children`, so accepting one
/// keeps the menu open on its contents.
fn path_candidates(word: &str, ctx: &ShellCompleteCtx<'_>) -> Vec<Candidate> {
    let (dir_part, name_part) = match word.rfind('/') {
        Some(i) => (&word[..=i], &word[i + 1..]),
        None => ("", word),
    };
    let dir_fs = resolve_dir(dir_part, ctx);
    let Ok(rd) = std::fs::read_dir(&dir_fs) else {
        return Vec::new();
    };
    let mut entries: Vec<(bool, String)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with(name_part) {
                return None;
            }
            if name.starts_with('.') && !name_part.starts_with('.') {
                return None;
            }
            // `path().is_dir()` follows symlinks; `file_type()` would not.
            Some((e.path().is_dir(), name))
        })
        .collect();
    // Directories first, then by name.
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    entries
        .into_iter()
        .map(|(is_dir, name)| Candidate {
            display: if is_dir { format!("{name}/") } else { name.clone() },
            insert: format!("{dir_part}{name}{}", if is_dir { "/" } else { " " }),
            desc: String::new(),
            has_children: is_dir,
        })
        .collect()
}

/// The directory `dir_part` (`""`, `src/`, `~/x/`, `/abs/`) names on disk.
fn resolve_dir(dir_part: &str, ctx: &ShellCompleteCtx<'_>) -> PathBuf {
    if dir_part.is_empty() {
        return ctx.cwd.to_path_buf();
    }
    let expanded: PathBuf = match (dir_part.strip_prefix("~/"), ctx.home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(dir_part),
    };
    if expanded.is_absolute() {
        expanded
    } else {
        ctx.cwd.join(expanded)
    }
}

/// Every executable file name on `$PATH`, sorted and deduplicated. Scanned
/// once per session by the caller; a new install needs a new murmur, the
/// same as a new shell.
pub fn scan_path_bins() -> Vec<String> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    let mut set = std::collections::BTreeSet::new();
    for dir in std::env::split_paths(&path) {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if is_executable_file(&p) {
                set.insert(e.file_name().to_string_lossy().into_owned());
            }
        }
    }
    set.into_iter().collect()
}

#[cfg(unix)]
fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn is_executable_file(p: &Path) -> bool {
    p.is_file()
        && p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "exe" | "cmd" | "bat" | "com"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tempdir with: dirs `docs/`, `src/` (holding `main.rs`); files
    /// `Cargo.toml`, `README.md`, `.hidden`.
    fn tree() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("docs")).unwrap();
        std::fs::create_dir(t.path().join("src")).unwrap();
        std::fs::write(t.path().join("src/main.rs"), "").unwrap();
        std::fs::write(t.path().join("Cargo.toml"), "").unwrap();
        std::fs::write(t.path().join("README.md"), "").unwrap();
        std::fs::write(t.path().join(".hidden"), "").unwrap();
        t
    }

    fn bins() -> Vec<String> {
        ["cargo", "cat", "ls"].map(String::from).to_vec()
    }

    fn ctx<'a>(t: &'a tempfile::TempDir, bins: &'a [String]) -> ShellCompleteCtx<'a> {
        ShellCompleteCtx { cwd: t.path(), path_bins: bins, home: Some(t.path()) }
    }

    fn inserts(v: &[Candidate]) -> Vec<String> {
        v.iter().map(|c| c.insert.clone()).collect()
    }

    #[test]
    fn first_word_completes_command_names_by_prefix() {
        let t = tree();
        let b = bins();
        assert_eq!(inserts(&candidates("!ca", &ctx(&t, &b))), ["!cargo ", "!cat "]);
        assert!(candidates("!", &ctx(&t, &b)).is_empty(), "no prefix, no list");
        assert!(candidates("!zz", &ctx(&t, &b)).is_empty());
    }

    #[test]
    fn later_words_list_the_cwd_directories_first_with_a_slash() {
        let t = tree();
        let b = bins();
        let out = candidates("!ls ", &ctx(&t, &b));
        assert_eq!(inserts(&out), ["!ls docs/", "!ls src/", "!ls Cargo.toml ", "!ls README.md "]);
        assert!(out[0].has_children && !out[2].has_children);
        assert_eq!(out[0].display, "docs/");
    }

    #[test]
    fn hidden_entries_only_when_the_prefix_starts_with_a_dot() {
        let t = tree();
        let b = bins();
        assert!(candidates("!ls ", &ctx(&t, &b)).iter().all(|c| !c.display.starts_with('.')));
        assert_eq!(inserts(&candidates("!ls .", &ctx(&t, &b))), ["!ls .hidden "]);
    }

    #[test]
    fn descends_into_a_directory_and_keeps_the_head() {
        let t = tree();
        let b = bins();
        assert_eq!(inserts(&candidates("!cat src/", &ctx(&t, &b))), ["!cat src/main.rs "]);
        assert_eq!(inserts(&candidates("!cat -n src/m", &ctx(&t, &b))), ["!cat -n src/main.rs "]);
    }

    #[test]
    fn tilde_expands_to_home() {
        let t = tree();
        let b = bins();
        assert_eq!(inserts(&candidates("!ls ~/d", &ctx(&t, &b))), ["!ls ~/docs/"]);
    }

    #[test]
    fn an_absolute_directory_is_used_as_is() {
        let t = tree();
        let b = bins();
        let abs = format!("!ls {}/s", t.path().display());
        let out = candidates(&abs, &ctx(&t, &b));
        assert_eq!(out.len(), 1);
        assert!(out[0].insert.ends_with("/src/"), "{}", out[0].insert);
    }

    #[test]
    fn the_list_is_capped_and_multiline_input_has_none() {
        let t = tree();
        let b = bins();
        for i in 0..12 {
            std::fs::write(t.path().join(format!("f{i:02}")), "").unwrap();
        }
        assert_eq!(candidates("!ls f", &ctx(&t, &b)).len(), MAX_MENU_ROWS);
        assert!(candidates("!ls\nf", &ctx(&t, &b)).is_empty());
        assert!(candidates("ls f", &ctx(&t, &b)).is_empty(), "no leading !");
    }

    #[test]
    fn scan_path_bins_reads_the_env_path() {
        let t = tempfile::tempdir().unwrap();
        let exe = t.path().join("mytool");
        std::fs::write(&exe, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(t.path().join("notes.txt"), "").unwrap();
        // Process-wide env: this test owns PATH for its duration.
        let saved = std::env::var_os("PATH");
        unsafe { std::env::set_var("PATH", t.path()) };
        let bins = scan_path_bins();
        unsafe {
            match saved {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }
        #[cfg(unix)]
        assert_eq!(bins, ["mytool"]);
        #[cfg(windows)]
        assert!(bins.is_empty(), "no .exe in the dir");
    }
}
```

- [x] **2.2 Register the module** — in `mur-core/src/cmd/agent/cli/mod.rs`, next to the existing `mod complete;` line add `mod shell_complete;`.

- [x] **2.3 Watch it pass** — `cargo nextest run -p mur-core --lib -E 'test(shell_complete)'`. Expected: 8 tests pass. (They are written together with the code; the red step for this task is the `mod shell_complete;` line failing to compile before the file exists — run 2.3 once with the file empty if you want to see it.) If `scan_path_bins_reads_the_env_path` is flaky under nextest's process-per-test model it cannot be — each test is its own process — but if it is ever run under plain `cargo test`, mark it `#[serial]` only if the crate already depends on `serial_test`; otherwise leave it.

- [x] **2.4 fmt + clippy**, then **commit**:

```
feat(murmur): shell_complete — candidates for ! lines

Pure over a cwd, a command list and a home: first word completes command
names, later words complete paths (dirs first, trailing slash, hidden only
behind a dot, ~ expanded), capped at MAX_MENU_ROWS.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 3 — wire the menu, cache PATH, update `/help`

**Interfaces.**
- Consumes: `shell_complete::{candidates, scan_path_bins, ShellCompleteCtx}` (Task 2); `complete::compute(input, skills, ctx, current)` and `CompletionState { items, selected, spaced, current }` (exist after #1255); `help_text()` (exists after #1256).
- Produces: `App::path_bins(&mut self) -> &[String]`; `fn refresh_completion(app)` dispatching on the first character; `completion_accept` reusing it.

### Steps

- [x] **3.1 Write the failing wiring test** — append to `mod shell_turn_tests` in `mod.rs`:

```rust
    /// `!` lines open the shell menu through the same refresh path as `/`,
    /// and accepting a directory keeps it open on that directory.
    #[test]
    fn bang_lines_get_the_shell_menu_and_directories_descend() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("docs")).unwrap();
        std::fs::write(t.path().join("docs/a.md"), "").unwrap();
        let mut app = App::test_fixture();
        app.cwd = Some(t.path().to_path_buf());
        app.path_bins = Some(vec!["cargo".into(), "cat".into()]);

        app.set_input("!ca");
        refresh_completion(&mut app);
        let items: Vec<String> = app.completion.as_ref().unwrap().items.iter().map(|c| c.display.clone()).collect();
        assert_eq!(items, ["cargo", "cat"]);
        assert_eq!(app.completion.as_ref().unwrap().current, None);

        app.set_input("!ls ");
        refresh_completion(&mut app);
        assert_eq!(app.completion.as_ref().unwrap().items[0].display, "docs/");
        completion_accept(&mut app);
        assert_eq!(app.input_text(), "!ls docs/");
        let inside = app.completion.as_ref().expect("menu stays open on a directory");
        assert_eq!(inside.items[0].display, "a.md");

        app.set_input("/skin ");
        refresh_completion(&mut app);
        assert!(app.completion.as_ref().unwrap().items.iter().any(|c| c.display == "mur"), "slash menu untouched");
    }
```

- [x] **3.2 Watch it fail** — `cargo nextest run -p mur-core --lib -E 'test(bang_lines_get_the_shell_menu)'`. Expected: compile error `no field path_bins on App`.

- [x] **3.3 Cache PATH on `App`** — in `app.rs`, next to `pub menu_ctx: super::complete::MenuContext,` add:

```rust
    /// Executable names on `$PATH` for `!` completion. `None` until the first
    /// `!` completion asks; scanned once per session after that.
    pub path_bins: Option<Vec<String>>,
```

  initialise it as `path_bins: None,` wherever `menu_ctx:` is initialised (the constructor and `test_fixture`), and add the accessor next to `current_values`:

```rust
    /// The `$PATH` executable list, scanned on first use.
    pub fn path_bins(&mut self) -> &[String] {
        self.path_bins
            .get_or_insert_with(super::shell_complete::scan_path_bins)
            .as_slice()
    }
```

- [x] **3.4 Dispatch in `refresh_completion`** — in `mod.rs` replace the fn with:

```rust
/// Recompute the completion menu from the current input. Called after every
/// edit and when Tab is pressed with the menu closed. `/` lines get the
/// command menu, `!` lines the shell menu, anything else none.
fn refresh_completion(app: &mut App) {
    let input = app.input_text();
    app.completion = if input.trim_start().starts_with('!') {
        shell_completion(app, input.trim_start())
    } else {
        complete::compute(&input, &app.skills, &app.menu_ctx, &app.current_values())
    };
}

/// The shell menu for a `!` line: commands for the first word, paths after.
/// Never marks a `current` row — a path has no value in force.
fn shell_completion(app: &mut App, line: &str) -> Option<complete::CompletionState> {
    let cwd = app
        .cwd
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let home = dirs::home_dir();
    let bins = app.path_bins().to_vec();
    let items = shell_complete::candidates(
        line,
        &shell_complete::ShellCompleteCtx {
            cwd: &cwd,
            path_bins: &bins,
            home: home.as_deref(),
        },
    );
    if items.is_empty() {
        return None;
    }
    Some(complete::CompletionState {
        items,
        selected: 0,
        spaced: false,
        current: None,
    })
}
```

  If `PathBuf` is not already imported at the top of `mod.rs`, add `use std::path::PathBuf;`.

- [x] **3.5 `completion_accept` reuses the dispatch** — replace its tail (from `app.set_input(&insert);` to the end of the fn) with:

```rust
    app.set_input(&insert);
    if descend {
        refresh_completion(app);
    } else {
        app.completion = None;
    }
}
```

- [x] **3.6 `/help` wording** — in `help_text()` change the `!cmd` row to:

```rust
        "  !cmd      run a local shell command; its output is sent to the agent as your message · Tab completes commands and paths",
```

- [x] **3.7 Watch it pass** — `cargo nextest run -p mur-core --lib -E 'test(bang_lines_get_the_shell_menu) | test(help_matches) | test(every_command_is_parsed) | test(/complete::/)'`. Expected: all pass. Then fmt + clippy.

- [x] **3.8 Commit**:

```
feat(murmur): live shell completion on ! lines

refresh_completion dispatches on the first character: `/` keeps the
command menu, `!` gets shell_complete (PATH names scanned once per session,
then cwd-relative paths; directories descend through the existing
has_children accept path). /help says so.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## After the last task

- Open one PR for the branch (title `feat(murmur): !cmd output is a turn; shell completion`), body listing the three commits and the manual checks below.
- Manual checks in a real terminal after `build.sh --install`: `!ls` while idle → agent replies to the listing; `!ls` while the agent is mid-reply → transcript shows `↗ steering: $ ls output`; `!ca<Tab>`, `!ls <Tab>`, then Tab on a directory descends.
- Docs via the `update-docs` skill: README `!cmd` line; docs-site agent-cli page.
