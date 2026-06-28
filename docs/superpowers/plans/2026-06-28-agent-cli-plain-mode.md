# mur agent cli — Plain / Screen-Reader Mode — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `mur agent cli` usable without the full-screen ratatui TUI — for screen readers, piping, and CI logs — via a `--plain` flag and an enriched line-based loop that still shows every tool step and lets an interactive user approve tools.

**Architecture:** `run_plain` (mod.rs:~1000) already exists and already runs automatically when stdout is not a TTY: a stdin→`dial_message_streaming`→stdout loop. This plan (1) adds a `--plain` flag so the loop can be forced on a TTY, and (2) enriches `run_plain` to print tool-step lines (the Glass Box transparency point — it currently drops them), a per-turn usage/cost footer line, an echoed prompt, and — when stdin is interactive — a real `[y/a/n]` HITL prompt instead of the current blanket auto-deny.

**Tech Stack:** Rust (edition 2024), `std::io` (line I/O, `IsTerminal`), the existing `a2a_dial::dial_message_streaming` + `StepEvent`, `footer::Pricing`/`load_pricing`. No ratatui, no crossterm, no new dependency.

## Global Constraints

- **Independent cli feature** — branch from `main` (this plan was written on `feat/agent-cli-plain-mode`, already cut off `main 86d9cdd1`). All edits are in `mur-core`.
- **Reuse, don't duplicate** — `run_plain` EXISTS (mod.rs ~1000-1075). Enrich it in place; do NOT write a second plain loop.
- **Rust edition 2024**; no hardcoded values (cost rate already comes from `footer::Pricing`); brand "MUR" uppercase in any new user-facing copy.
- **Tests:** mur-core needs `ORT_STRATEGY=download`; toolchain cargo if rustup broken (`export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$HOME/.cargo/bin:$PATH"`, plain `cargo test`).
- **Lint gate:** `cargo clippy -p mur-core -- -D warnings` + `cargo fmt`.
- **TUI-safety is moot here** — plain mode owns plain stdout/stdin (no alt-screen), so `println!`/`eprintln!` and blocking stdin reads are fine; that's the whole point.
- **`run_plain` runs on a blocking thread** (`cmd_cli` calls it via `spawn_blocking`), so blocking stdin reads inside it (and inside its HITL callback) are correct.

---

### Task 1: `--plain` flag forces the plain loop on a TTY

**Files:**
- Modify: `mur-core/src/cli/agent.rs` (the `Cli { … }` clap variant, ~91-104)
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (`cmd_cli`, ~91-126) + its dispatch call site
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (`run_plain` signature gains `interactive: bool`)

**Interfaces:**
- Produces: `cmd_cli(names, resume, auto, skin, plain)`; `run_plain(home, agent, auto, interactive)`. Plain loop runs when `plain || !io::stdout().is_terminal()`.

- [ ] **Step 1: Add the flag to the clap variant** (`cli/agent.rs`, in the `Cli { … }` variant after `auto`)

```rust
    /// Plain line-based output (no full-screen TUI) — for screen readers,
    /// piping, and CI logs. Auto-enabled when stdout is not a terminal.
    #[arg(long)]
    plain: bool,
```

- [ ] **Step 2: Thread it through the dispatch call site**

Grep for where the `Cli` variant is destructured and `cmd_cli(` is called (likely `mur-core/src/cli/agent.rs` or a `dispatch.rs`): `grep -rn "cmd_cli(" mur-core/src`. Add `plain` to the destructure and pass it:
```rust
        Agent::Cli { names, resume, auto, skin, plain } => {
            cli::cmd_cli(&names, resume, auto, skin, plain).await
        }
```
(Match the real variant/enum names; the point is the new `plain` field reaches `cmd_cli`.)

- [ ] **Step 3: Branch on the flag in `cmd_cli`** (mod.rs ~117-125)

Change the existing non-TTY gate to also fire on `--plain`, and pass an `interactive` flag (true only on a real stdin TTY) into `run_plain`:
```rust
    // Plain line mode: forced by --plain, or automatic when stdout is not a
    // terminal (piped / CI). `interactive` drives the prompt + HITL behaviour:
    // a real stdin TTY gets an echoed prompt and a [y/a/n] HITL question; a
    // pipe gets neither.
    if plain || !io::stdout().is_terminal() {
        let home2 = home.clone();
        let agent2 = agent.clone();
        let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
        return tokio::task::spawn_blocking(move || run_plain(&home2, &agent2, auto, interactive))
            .await?;
    }
```
Update `cmd_cli`'s signature to take `plain: bool` (add the param).

- [ ] **Step 4: Update `run_plain`'s signature** — `fn run_plain(home: &Path, agent: &str, auto: bool, interactive: bool) -> Result<()>`. (Tasks 2-3 use `interactive`; for this task just thread it and, if clippy flags it unused, add a temporary `let _ = interactive;` that Task 2 removes.)

- [ ] **Step 5: Build + lint + commit**

Run: `ORT_STRATEGY=download cargo check -p mur-core && cargo clippy -p mur-core -- -D warnings && cargo fmt`
Manually verify the flag exists: `cargo run -p mur-core -- agent cli --help 2>&1 | grep -A1 plain` (expect the `--plain` line).

```bash
git add mur-core/src/cli/agent.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): --plain flag forces the line-based loop on a TTY"
```

---

### Task 2: Plain loop shows tool steps + a usage footer + an echoed prompt

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (`run_plain`)

**Interfaces:**
- Consumes: `interactive` (Task 1), `footer::Pricing` + `load_pricing` (existing), `crate::a2a_dial::StepEvent` (existing).

- [ ] **Step 1: Restructure the stdin loop to per-read locking** (enables Task 3's HITL read)

Replace the `for line in stdin.lock().lines()` loop (which holds the lock for the whole turn) with a manual loop that locks only to read one line, so the in-turn HITL callback (Task 3) can read stdin too:
```rust
    let mut out = io::stdout();
    let mut context: Option<String> = None;
    let pricing = load_pricing(home, agent);
    loop {
        if interactive {
            let _ = write!(out, "you › ");
            let _ = out.flush();
        }
        let mut line = String::new();
        // Lock only for the read so the HITL callback can read stdin mid-turn.
        if io::stdin().lock().read_line(&mut line)? == 0 {
            break; // EOF (Ctrl-D / end of pipe)
        }
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        let task_id = uuid::Uuid::now_v7().to_string();
        let params = build_params(text, &task_id, context.as_deref(), None);
        let streamed = std::cell::Cell::new(false);
        // ... dial (Step 2) ...
    }
    Ok(())
```
(Drop the now-unused `use std::io::BufRead;`/`stdin.lock().lines()` form; keep `use std::io::Write;`.)

- [ ] **Step 2: Print tool-step lines** (replace the `|_step| {}` step callback)

The step callback currently discards every step. Print a line per step so plain mode keeps the Glass Box transparency:
```rust
            |step| {
                match step {
                    crate::a2a_dial::StepEvent::Started { name, args, .. } => {
                        // One-line arg hint: the command for bash, else compact JSON.
                        let hint = args
                            .get("command")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| args.to_string());
                        let hint: String = hint.chars().take(PLAIN_STEP_HINT_MAX).collect();
                        let _ = writeln!(out2.borrow_mut(), "→ {name} {hint}");
                        let _ = out2.borrow_mut().flush();
                    }
                    crate::a2a_dial::StepEvent::Completed { name, ok, duration_ms, .. } => {
                        let glyph = if ok { '✔' } else { '✗' };
                        let _ = writeln!(out2.borrow_mut(), "{glyph} {name} · {duration_ms}ms");
                        let _ = out2.borrow_mut().flush();
                    }
                }
            },
```
Add `const PLAIN_STEP_HINT_MAX: usize = 120;` near the top of the file (no hardcoded literal inline). Because `out` is borrowed by multiple closures, wrap it once: `let out2 = std::cell::RefCell::new(io::stdout());` and have the text + step callbacks write through `out2.borrow_mut()` (the text-delta closure too). Use `.chars().take(...)` (char-safe truncation — never byte-slice, to avoid a CJK panic). Keep the text-delta closure printing reply text (and still skipping `thinking`).

- [ ] **Step 3: Print a usage footer line after each turn** (on success) — REUSE the footer helpers

footer.rs already has `parse_usage(&Value) -> UsageCounts { input, output }` and `turn_cost(&Pricing, &UsageCounts) -> Option<f64>` (both already tested). Reuse them — do NOT write a new cost fn. In the `Ok(task)` → `Ok((reply, tid))` arm, after writing the reply + newline:
```rust
                    // Usage footer: total tokens + cost (reuse footer helpers).
                    if let Some(usage) = task.get("usage") {
                        let u = footer::parse_usage(usage);
                        match footer::turn_cost(&pricing, &u) {
                            Some(c) => { let _ = writeln!(out2.borrow_mut(), "  {} tok · ${:.3}", u.input + u.output, c); }
                            None => { let _ = writeln!(out2.borrow_mut(), "  {} tok", u.input + u.output); }
                        }
                    }
```

- [ ] **Step 4: Build + lint**

No new unit test — Step 2/3 are step-line formatting + reuse of the already-tested `parse_usage`/`turn_cost`; the loop is manual-verified (stdin/stdout). Don't invent a fake test.
Run: `ORT_STRATEGY=download cargo check -p mur-core && ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli 2>&1 | grep "test result" && cargo clippy -p mur-core -- -D warnings && cargo fmt`

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): plain mode prints tool steps + a usage footer + an echoed prompt"
```

---

### Task 3: Interactive HITL prompt in plain mode

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (`run_plain` HITL callback)

**Interfaces:**
- Consumes: `interactive` (Task 1), `auto` (existing param), `dial_method` + `DialMode` (existing, already used by the current auto-resolve).

- [ ] **Step 1: Make the HITL callback prompt when interactive**

The current HITL callback blanket auto-denies (or auto-approves under `--auto`). Replace it so that, when `interactive` and not `--auto`, it asks on stdin; otherwise it keeps the current non-interactive auto-resolve:
```rust
            |hitl| {
                let id = hitl.get("hitl_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let tool = hitl.get("tool_name").and_then(|v| v.as_str()).unwrap_or("tool");
                let allow = if auto {
                    true
                } else if interactive {
                    // Ask on the same stdin (the outer loop isn't holding the lock
                    // during the dial). Default to deny on EOF/blank/unknown.
                    let mut o = io::stdout();
                    let _ = write!(o, "  tool approval: {tool} — [y]es / [a]lways / [n]o? ");
                    let _ = o.flush();
                    let mut ans = String::new();
                    let _ = io::stdin().lock().read_line(&mut ans);
                    match ans.trim().chars().next() {
                        Some('y') | Some('Y') | Some('a') | Some('A') => true,
                        _ => false,
                    }
                } else {
                    eprintln!("[non-interactive: auto-{} tool approval (use --auto to allow)]",
                        if auto { "approving" } else { "denying" });
                    auto
                };
                let _ = dial_method(
                    home, agent, "tool/hitl_respond",
                    serde_json::json!({ "hitl_id": id, "allow": allow }),
                    DialMode::RequireRunning,
                );
            },
```
> `[a]lways` returning `true` (approve this once) is the lazy v1 — a true session-allow set would need to thread state into the callback; YAGNI for plain v1. **ponytail:** `[a]` approves once like `[y]`; add a session allow-set if it's actually wanted. Note this in the report so the reviewer doesn't flag it as a bug.

- [ ] **Step 2: Build + lint**

Run: `ORT_STRATEGY=download cargo check -p mur-core && cargo clippy -p mur-core -- -D warnings && cargo fmt`
Confirm existing cli tests pass: `ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli 2>&1 | grep "test result"`.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): plain mode prompts [y/a/n] for tool approval when interactive"
```

---

## Manual verification (after all tasks)

1. Build: `cargo build --release -p mur-core`.
2. **Piped (auto):** `echo "what is 2+2?" | ./target/release/mur agent cli <agent>` → prints the reply (+ any tool lines) + a `N tok` footer, no prompt, HITL auto-resolved. (Existing behavior, now with steps + footer.)
3. **Interactive --plain:** `./target/release/mur agent cli <agent> --plain` → `you › ` prompt; type a request that runs a tool → see `→ bash …` / `✔ bash … · Nms` lines; on an ask-policy agent see `tool approval: bash — [y/a/n]?` and answering `y` runs it; reply + `N tok · $X` footer; Ctrl-D exits.
4. **Screen-reader sanity:** no escape/cursor sequences in the output (pipe to `cat -v` and confirm clean text).

## Out of scope (ponytail)

- A session allow-set for `[a]lways` (v1 approves once).
- Reasoning/thinking deltas in plain output (kept suppressed — steps + reply are the transparency; a `--plain-reasoning` toggle is a later add if wanted).
- Slash commands, images, scrollback, steering in plain mode (the TUI owns those).

## Self-Review (completed)

- **Spec coverage:** `--plain` flag + force (T1), steps + footer + echo + stdin-relock (T2), interactive HITL (T3). ✔
- **Placeholder scan:** none — code in every step; the `[a]`-approves-once and `PLAIN_STEP_HINT_MAX` are explicit ponytail decisions, not gaps. ✔
- **Type consistency:** `interactive` (T1) consumed by T2/T3; `out2: RefCell<Stdout>` shared by the delta + step closures; `plain_cost`/`Pricing` fields match `load_pricing`. ✔
