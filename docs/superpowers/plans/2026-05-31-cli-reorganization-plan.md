# CLI Reorganization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce mur CLI from 45 top-level commands to 23 (11 noun groups + 12 standalone) by removing dead code, grouping related commands under noun parents, and updating outdated descriptions.

**Architecture:** Git-style noun-verb grouping. Six phases: (1) remove dead code, (2) scaffold new groups, (3) move commands with deprecation passthrough, (4) cross-project reference scan, (5) update descriptions/internals, (6) verify.

**Tech Stack:** Rust 2024, clap derive, anyhow

**Spec:** `docs/superpowers/specs/2026-05-31-cli-reorganization-design.md`

---

### Task 1: Remove dead commands (Gc, Import, Community)

**Files:**
- Modify: `mur-core/src/cli/mod.rs` — remove Commands::Gc, Commands::Import, Commands::Community
- Modify: `mur-core/src/cli/actions.rs` — remove CommunityAction, PackAction
- Modify: `mur-core/src/dispatch.rs` — remove dispatch branches
- Delete: `mur-core/src/cmd/community_cmd.rs`
- Modify: `mur-core/src/cmd/mod.rs` — remove `mod community_cmd`
- Modify: `mur-core/src/cmd/misc.rs` — remove cmd_gc function

- [ ] **Step 1: Remove Gc variant from Commands enum**

In `mur-core/src/cli/mod.rs`, remove lines 96-101:
```rust
    /// Garbage collect low-quality patterns
    Gc {
        /// Auto-archive without prompting
        #[arg(long)]
        auto: bool,
    },
```

- [ ] **Step 2: Remove Import variant from Commands enum**

In `mur-core/src/cli/mod.rs`, remove lines 232-239:
```rust
    /// Import rules from AI tool config files (.cursorrules, CLAUDE.md, etc.)
    Import {
        /// Files to import (auto-detects if not specified)
        #[arg(long)]
        file: Option<Vec<String>>,
        /// Preview what would be imported without saving
        #[arg(long)]
        dry_run: bool,
    },
```

- [ ] **Step 3: Remove Community variant from Commands enum**

In `mur-core/src/cli/mod.rs`, remove lines 166-170:
```rust
    /// Community publish/fetch
    Community {
        #[command(subcommand)]
        action: CommunityAction,
    },
```

Also remove the `use` import of `CommunityAction` if it's the only reference (check line ~12 of mod.rs).

- [ ] **Step 4: Remove CommunityAction and PackAction from actions.rs**

In `mur-core/src/cli/actions.rs`, remove:
- Lines 234-268 (CommunityAction enum with all variants: Publish, Fetch, Search, List, Star, Report, Packs, Pack)
- Lines 299-311 (PackAction enum)

- [ ] **Step 5: Remove dispatch branches in dispatch.rs**

In `mur-core/src/dispatch.rs`:

Remove the Gc dispatch (line ~57):
```rust
        Commands::Gc { auto } => cmd::misc::cmd_gc(auto)?,
```

Remove the Import dispatch — find with `grep -n "Import" mur-core/src/dispatch.rs`. Remove the entire match arm.

Remove the Community dispatch (lines ~189-209) — the entire match block for `Commands::Community { action }`.

Also remove any `use` imports for community_cmd at the top of dispatch.rs:
```rust
// Remove this line if present:
use crate::cmd::community_cmd;
```

- [ ] **Step 6: Remove cmd_gc function from misc.rs**

In `mur-core/src/cmd/misc.rs`, find and remove the `cmd_gc` function (around line 133):
```rust
pub(crate) fn cmd_gc(_auto: bool) -> Result<()> {
    eprintln!(
        "# mur gc: pattern lifecycle management removed -- use `mur skill sweep` for skill lifecycle."
    );
    Ok(())
}
```

- [ ] **Step 7: Delete community_cmd.rs and remove its module declaration**

```bash
rm mur-core/src/cmd/community_cmd.rs
```

In `mur-core/src/cmd/mod.rs`, remove:
```rust
pub(crate) mod community_cmd;
```

- [ ] **Step 8: Build check**

```bash
cargo build -p mur-core 2>&1 | head -50
```

Expected: compiles without errors (some warnings about unused imports may remain — fix those).

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "refactor(cli): remove dead commands — gc, import, community

- Gc: dead stub, replaced by `mur skill sweep`
- Import: dead code, replaced by `mur notes ingest`
- Community: public marketplace, conflicts with strategy; use `mur skill publish/install`

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Add AuthAction and DaemonAction enums

**Files:**
- Modify: `mur-core/src/cli/actions.rs`
- Modify: `mur-core/src/cli/mod.rs`

- [ ] **Step 1: Add AuthAction enum to actions.rs**

After the `HookEvent` enum (line 35), insert:
```rust
#[derive(Subcommand)]
pub enum AuthAction {
    /// Log in to mur.run
    Login,
    /// Log out and clear stored credentials
    Logout,
}
```

- [ ] **Step 2: Add DaemonAction enum to actions.rs**

After the new `AuthAction`, insert:
```rust
#[derive(Subcommand)]
pub enum DaemonAction {
    /// Start the murmurd daemon
    Start {
        /// Run in background (detach from terminal)
        #[arg(long)]
        detach: bool,
    },
    /// Stop the murmurd daemon
    Stop,
    /// Show murmurd daemon status
    Status,
    /// Start the local API server for the web dashboard
    Serve {
        /// Port to listen on
        #[arg(long, default_value = "3847")]
        port: u16,
        /// Open browser after starting
        #[arg(long)]
        open: bool,
        /// Read-only mode (reject all write operations)
        #[arg(long)]
        readonly: bool,
    },
    /// Configure the daemon sleep cycle
    Sleep {
        #[command(subcommand)]
        action: SleepAction,
    },
}
```

- [ ] **Step 3: Add Auth and Daemon to Commands enum in mod.rs**

In `mur-core/src/cli/mod.rs`:
- After `Commands::Login` and `Commands::Logout` (lines 176-179), replace them with:
```rust
    /// Authentication (login / logout)
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
```

- After the `Auth` variant, add:
```rust
    /// Background daemon and server management
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
```

- Remove the old `Login` and `Logout` variants (they're replaced by the `Auth` group above)
- Leave `Commands::Murmurd`, `Commands::Serve`, and `Commands::Sleep` in place for now (they become hidden passthroughs in Task 3)

- [ ] **Step 4: Update imports in mod.rs**

Ensure `AuthAction` and `DaemonAction` are imported. In `mur-core/src/cli/mod.rs`, update the import from `actions` (around line 12-25):
```rust
use crate::cli::actions::{
    AuthAction, ChatAction, ConversationsAction, DaemonAction, DeployAction, DraftsAction,
    EvalAction, ExchangeAction, HookEvent, InternalsAction, MurmurdAction, ProjectAction,
    SessionAction, SleepAction, SyncAction, TeamAction, WorkflowAction,
};
```

- [ ] **Step 5: Build check**

```bash
cargo build -p mur-core 2>&1 | head -50
```

Expected: Warnings about unused variants (AuthAction, DaemonAction not yet dispatched — will be used in Task 3).

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(cli): add AuthAction and DaemonAction enums"
```

---

### Task 3: Move commands into new groups

This is the largest task. For each command move:
1. Add the subcommand to the target action enum
2. Add the new dispatch route
3. Mark the old Commands variant as hidden with deprecation warning dispatch

**Files:**
- Modify: `mur-core/src/cli/actions.rs` — SessionAction, HookEvent, WorkflowAction, ChatAction, SkillAction, InternalsAction
- Modify: `mur-core/src/cli/skill.rs` — SkillAction
- Modify: `mur-core/src/cli/notes.rs` — NotesAction
- Modify: `mur-core/src/dispatch.rs`
- Modify: `mur-core/src/cli/mod.rs`

- [ ] **Step 1: Session — add In, Out, Discard to SessionAction**

In `mur-core/src/cli/actions.rs`, add to `SessionAction` enum (after the `Push` variant at line 231):
```rust
    /// Start session recording and inject context (shorthand for start + context)
    In {
        /// Source identifier (e.g. claude-code)
        #[arg(long, default_value = "claude-code")]
        source: String,
    },
    /// Stop session recording with post-session menu
    Out {
        /// Action to perform: analyze, export, skip
        #[arg(long)]
        action: Option<String>,
        /// Force LLM analysis even for short sessions
        #[arg(long)]
        force: bool,
    },
    /// Stop recording and delete the session (no export)
    Discard,
```

- [ ] **Step 2: Dispatch — add route for SessionAction::In/Out/Discard**

In `mur-core/src/dispatch.rs`, find the existing `Commands::Session { action }` dispatch block. Add new match arms inside the existing `match action {}` block:
```rust
            SessionAction::In { source } => cmd::session::cmd_in(&source).await?,
            SessionAction::Out { action, force } => {
                cmd::session::cmd_out(action.as_deref(), force).await?
            }
            SessionAction::Discard => cmd::session::cmd_session_exit()?,
```

- [ ] **Step 3: Mark old Exit/Quit/In/Out as hidden passthroughs**

In `mur-core/src/cli/mod.rs`, for `Commands::Exit`, `Commands::Quit`, `Commands::In`, `Commands::Out`:

- Add `#[arg(hide = true)]` above each variant
- Keep the variants in the enum (they become hidden)
- In dispatch.rs, replace their dispatch with a deprecation message + forward:

```rust
        // Deprecated: use `mur session discard`
        Commands::Exit | Commands::Quit => {
            eprintln!("# mur exit/quit: use `mur session discard`");
            cmd::session::cmd_session_exit()?
        }
        // Deprecated: use `mur session in`
        Commands::In { source } => {
            eprintln!("# mur in: use `mur session in`");
            cmd::session::cmd_in(&source).await?
        }
        // Deprecated: use `mur session out`
        Commands::Out { action, force } => {
            eprintln!("# mur out: use `mur session out`");
            cmd::session::cmd_out(action.as_deref(), force).await?
        }
```

- [ ] **Step 4: Auth — add dispatch for Commands::Auth**

In `mur-core/src/dispatch.rs`, add after the existing Login/Logout dispatch:
```rust
        Commands::Auth { action } => match action {
            AuthAction::Login => cmd::misc::cmd_login().await?,
            AuthAction::Logout => cmd::misc::cmd_logout().await?,
        },
```

Then mark old `Commands::Login` and `Commands::Logout` as hidden passthroughs (similar to Exit/Quit pattern above). In dispatch.rs:
```rust
        // Deprecated: use `mur auth login`
        Commands::Login => {
            eprintln!("# mur login: use `mur auth login`");
            cmd::misc::cmd_login().await?
        }
        // Deprecated: use `mur auth logout`
        Commands::Logout => {
            eprintln!("# mur logout: use `mur auth logout`");
            cmd::misc::cmd_logout().await?
        }
```

Also add `#[arg(hide = true)]` above the old Login/Logout variants in mod.rs.

- [ ] **Step 5: Hook — add Inject and Context to HookEvent**

In `mur-core/src/cli/actions.rs`, add to `HookEvent` enum (after `Stats` at line 34):
```rust
    /// Test injection pipeline: show what skills would be injected for a query
    Inject {
        /// Query to test injection against
        query: String,
    },
    /// Inject context-aware skills (auto-detects project/session context)
    Context {
        /// Quiet mode — only output injected skills
        #[arg(long, short)]
        quiet: bool,
        /// Compact output
        #[arg(long)]
        compact: bool,
        /// Override auto-detected query
        #[arg(long)]
        query: Option<String>,
        /// Write context to ~/.mur/context.md
        #[arg(long)]
        file: bool,
        /// Token budget (default: 2000)
        #[arg(long, default_value = "2000")]
        budget: usize,
        /// Source tool identifier
        #[arg(long, default_value = "cli")]
        source: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Scope filter (repeatable key=value)
        #[arg(long)]
        scope: Vec<String>,
    },
```

Add dispatch in the `Commands::Hook { event }` match block:
```rust
            HookEvent::Inject { query } => cmd::inject_cmd::cmd_inject(&query).await?,
            HookEvent::Context {
                quiet,
                compact,
                query,
                file,
                budget,
                source,
                json,
                scope,
            } => {
                cmd::context::cmd_context(
                    query, compact, file, budget, source, json_output, scope, quiet,
                )
                .await?
            }
```

Wait — check the actual `cmd_context` function signature. Let me verify.

**Check:** In `mur-core/src/cmd/context.rs:11`, the function is:
```rust
pub(crate) async fn cmd_context(
    query: Option<String>,
    compact: bool,
    write_file: bool,
    budget: usize,
    source: String,
    json_output: bool,
    scope_args: Vec<String>,
    quiet: bool,
) -> Result<()>
```

So the dispatch for HookEvent::Context should be:
```rust
            HookEvent::Context {
                quiet,
                compact,
                query,
                file,
                budget,
                source,
                json,
                scope,
            } => {
                cmd::context::cmd_context(
                    query, compact, file, budget, source, json, scope, quiet,
                )
                .await?
            }
```

Now mark old `Commands::Inject` and `Commands::Context` as `#[arg(hide = true)]` with deprecation dispatch.

- [ ] **Step 6: Workflow — add Run and Suggest to WorkflowAction**

In `mur-core/src/cli/actions.rs`, add to `WorkflowAction` enum (before `List` at line 91):
```rust
    /// Run a workflow by name or semantic query
    Run {
        /// Workflow name or search query
        query: String,
        /// Cancel remaining parallel branches on first failure
        #[arg(long)]
        fail_fast: bool,
        /// Print workflow as AI prompt instead of executing
        #[arg(long)]
        prompt: bool,
    },
    /// Show workflow composition suggestions and pending nudges
    Suggest {
        /// Auto-create suggested workflows as drafts
        #[arg(long)]
        create: bool,
        /// Accept a pending nudge by id
        #[arg(long, value_name = "ID")]
        accept: Option<String>,
        /// Dismiss a pending nudge by id
        #[arg(long, value_name = "ID")]
        dismiss: Option<String>,
    },
```

Add dispatch in the `Commands::Workflow { action }` match block:
```rust
            WorkflowAction::Run {
                query,
                fail_fast,
                prompt,
            } => cmd::workflow::cmd_workflow_run(&query, fail_fast, prompt).await?,
            WorkflowAction::Suggest {
                create,
                accept,
                dismiss,
            } => cmd::workflow_suggest::cmd_suggest(create, accept.as_deref(), dismiss.as_deref())?,
```

**Note:** Check the actual function name for suggest — it may be in `cmd::misc` or `cmd::workflow`. Search with:
```bash
grep -rn "cmd_suggest\|Suggest" mur-core/src/dispatch.rs | head -5
```

Mark old `Commands::Run` and `Commands::Suggest` as `#[arg(hide = true)]` with deprecation passthrough dispatch.

- [ ] **Step 7: Chat — merge ConversationsAction into ChatAction**

In `mur-core/src/cli/actions.rs`, add all `ConversationsAction` variants to `ChatAction`:

```rust
#[derive(Subcommand)]
pub enum ChatAction {
    // --- Existing chat browsing ---
    /// List days in the archive (Layer 1)
    List {
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        src: Option<String>,
    },
    /// Show a single day's summary (Layer 2)
    Show { date: String },
    /// Dump raw JSONL for a conversation (Layer 3)
    Raw { date: String, conv: String },
    /// Semantic + keyword search
    Search {
        query: String,
        #[arg(long, default_value = "10")]
        limit: usize,
        #[arg(long)]
        src: Option<String>,
    },
    /// Ask a natural-language question about your conversation archive
    Ask {
        /// Question to ask
        question: Option<String>,
        #[arg(long)]
        src: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        until: Option<String>,
        #[arg(long, default_value = "5")]
        k: usize,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        min_score: Option<f64>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        no_escalate: bool,
        #[arg(long)]
        debug_prompt: bool,
        #[arg(long)]
        strict_citations: bool,
        #[arg(long = "continue", conflicts_with = "new_flag")]
        continue_flag: bool,
        #[arg(long = "new", conflicts_with = "continue_flag")]
        new_flag: bool,
        #[arg(long, conflicts_with_all = ["continue_flag", "new_flag"])]
        show_session: bool,
        #[arg(long, conflicts_with = "summarize_model")]
        no_summarize: bool,
        #[arg(long)]
        summarize_model: Option<String>,
    },
    // --- From ConversationsAction ---
    /// Run polling ingesters (Cursor/Gemini/Aider)
    Pull,
    /// Apply retention cleanup
    Cleanup,
    /// Rebuild LanceDB from raw + summaries
    Reindex {
        #[arg(long, conflicts_with_all = ["spans_only", "rollups_only"])]
        raw_only: bool,
        #[arg(long, conflicts_with_all = ["raw_only", "rollups_only"])]
        spans_only: bool,
        #[arg(long, conflicts_with_all = ["raw_only", "spans_only"])]
        rollups_only: bool,
    },
    /// Run conversation archive health checks
    Doctor,
    /// Check migration preconditions
    Preflight,
    /// Migrate from commander paths
    Migrate {
        #[arg(long)]
        run: bool,
        #[arg(long, conflicts_with_all = &["run", "discard_staging"])]
        resume: bool,
        #[arg(long, conflicts_with_all = &["run", "resume"])]
        discard_staging: bool,
    },
    /// Roll back to commander's old paths
    Rollback,
    /// Generate hybrid summaries for completed days
    Compact {
        #[arg(long)]
        date: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        if_stale: bool,
        #[arg(long)]
        max_days: Option<u32>,
        #[arg(long)]
        extractive_only: bool,
        #[arg(long)]
        debug_prompt: bool,
        #[arg(long)]
        skip_rollups: bool,
    },
    /// Generate weekly + monthly rollup summaries
    Rollup {
        #[arg(long)]
        week: Option<String>,
        #[arg(long, conflicts_with = "week")]
        month: Option<String>,
        #[arg(long, conflicts_with_all = ["week", "month"])]
        all_missing: bool,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        if_stale: bool,
        #[arg(long)]
        max_weeks: Option<u32>,
        #[arg(long)]
        max_months: Option<u32>,
    },
    /// Aggregate LLM call telemetry into per-stage cost report
    CostReport {
        #[arg(long, default_value = "7d")]
        since: String,
        #[arg(long)]
        json: bool,
    },
}
```

In `dispatch.rs`, update the `Commands::Chat { action }` match to include all the new variants, routing them to their existing `cmd::conversations_cmd::*` functions.

Example for Pull:
```rust
            ChatAction::Pull => cmd::conversations_cmd::cmd_conversations_pull().await?,
```

Mark old `Commands::Conversations` and `Commands::Ask` as `#[arg(hide = true)]` with deprecation passthrough dispatch.

- [ ] **Step 8: Skill — add Exchange, Drafts, Eval to SkillAction**

In `mur-core/src/cli/skill.rs`, add to `SkillAction` (the existing enum at line 6):

```rust
    // --- Import/Export (from mur exchange) ---
    /// Import a single MKEF skill file
    #[command(name = "exchange-import")]
    ExchangeImport {
        /// Path to MKEF YAML file
        file: String,
    },
    /// Import all MKEF files from ~/.mur/exchange/
    #[command(name = "exchange-import-all")]
    ExchangeImportAll,
    /// Export a skill to MKEF format
    #[command(name = "exchange-export")]
    ExchangeExport {
        /// Skill name to export
        name: String,
        /// Output directory (default: ~/.mur/exchange/)
        #[arg(long)]
        dir: Option<String>,
    },
    // --- Drafts (from mur drafts) ---
    /// List pending skill drafts
    #[command(name = "drafts-list")]
    DraftsList {
        #[arg(long, default_value_t = 30)]
        since: u32,
    },
    /// Show a draft by id-prefix
    #[command(name = "drafts-show")]
    DraftsShow {
        id: String,
    },
    /// Accept a draft locally
    #[command(name = "drafts-accept")]
    DraftsAccept {
        id: String,
        #[arg(long = "as-tier")]
        as_tier: Option<String>,
    },
    /// Reject a draft
    #[command(name = "drafts-reject")]
    DraftsReject {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    // --- Eval (from mur eval) ---
    /// Run an eval suite against local skills
    #[command(name = "eval")]
    Eval {
        /// Suite name: retrieval | maturity | reflector | federation
        suite: String,
        /// Output format: text | json
        #[arg(long, default_value = "text")]
        format: String,
    },
```

**Note:** We use `#[command(name = "exchange-import")]` so the CLI surface reads `mur skill exchange-import`, `mur skill exchange-export`, `mur skill drafts-list`, etc. This keeps subcommand names flat (no nesting) while grouping logically under `mur skill`. The user types `mur skill exchange-import --help`.

Wait — re-reading the spec, the user just approved `mur skill exchange`, `mur skill drafts`, `mur skill eval`. Let me re-think. Actually, the spec says these get absorbed into `mur skill`. The subcommand naming should be clean.

Better approach: Use the existing `ExchangeAction`, `DraftsAction`, `EvalAction` enums but nest them under skill subcommands:

Actually, clap doesn't support true nested sub-sub-enums easily with `#[command(subcommand)]`. The simplest approach is to flatten them with descriptive names as shown above. Alternatively, keep ExchangeAction/DraftsAction as separate enums in actions.rs and add them as subcommand-bearing variants:

```rust
    /// Import/export skills in MKEF format
    Exchange {
        #[command(subcommand)]
        action: ExchangeAction,
    },
    /// Manage skill drafts
    Drafts {
        #[command(subcommand)]
        action: DraftsAction,
    },
    /// Run eval suites
    Eval {
        #[command(subcommand)]
        action: EvalAction,
    },
```

This IS possible — a variant of a Subcommand enum can itself contain a `#[command(subcommand)]`. This gives us:
```
mur skill exchange import <file>
mur skill exchange export <name>
mur skill drafts list
mur skill drafts accept <id>
mur skill eval run <suite>
```

This is cleaner. Let me use this approach.

In `mur-core/src/cli/skill.rs`, add these three variants to `SkillAction`:
```rust
    /// Import/export skills in MKEF format
    Exchange {
        #[command(subcommand)]
        action: crate::cli::actions::ExchangeAction,
    },
    /// Manage pending skill drafts
    Drafts {
        #[command(subcommand)]
        action: crate::cli::actions::DraftsAction,
    },
    /// Run eval suites
    Eval {
        #[command(subcommand)]
        action: crate::cli::actions::EvalAction,
    },
```

And update the imports in skill.rs accordingly.

In dispatch.rs, add to the `Commands::Skill { action }` match block:
```rust
            SkillAction::Exchange { action } => match action {
                ExchangeAction::Import { file } => cmd::exchange_cmd::cmd_exchange_import(&file).await?,
                ExchangeAction::ImportAll => cmd::exchange_cmd::cmd_exchange_import_all().await?,
                ExchangeAction::Export { name, dir } => {
                    cmd::exchange_cmd::cmd_exchange_export(&name, dir.as_deref()).await?
                }
            },
            SkillAction::Drafts { action } => match action {
                DraftsAction::List { since } => cmd::drafts::cmd_drafts_list(since).await?,
                DraftsAction::Show { id } => cmd::drafts::cmd_drafts_show(&id).await?,
                DraftsAction::Accept { id, as_tier } => {
                    cmd::drafts::cmd_drafts_accept(&id, as_tier.as_deref()).await?
                }
                DraftsAction::Reject { id, reason } => {
                    cmd::drafts::cmd_drafts_reject(&id, reason.as_deref()).await?
                }
            },
            SkillAction::Eval { action } => match action {
                EvalAction::Run { suite, format } => {
                    cmd::eval_cmd::cmd_eval_run(&suite, &format)?
                }
            },
```

**Check which files implement exchange/drafts/eval:**
```bash
grep -rn "pub.*fn cmd_exchange\|pub.*fn cmd_drafts\|pub.*fn cmd_eval" mur-core/src/cmd/ --include="*.rs"
```

Mark old `Commands::Exchange`, `Commands::Drafts`, `Commands::Eval` as `#[arg(hide = true)]` with deprecation passthrough.

- [ ] **Step 9: Notes — add Search to NotesAction**

In `mur-core/src/cli/notes.rs`, add to `NotesAction`:
```rust
    /// Search notes by keyword
    Search {
        /// Search query
        query: String,
        /// Max results
        #[arg(long, short = 'k', default_value_t = 8)]
        limit: usize,
        /// JSON output
        #[arg(long)]
        json: bool,
    },
```

In dispatch.rs, add to `Commands::Notes { action }` match:
```rust
            NotesAction::Search { query, limit, json } => {
                cmd::notes_cmd::cmd_search(&query, limit, json)?
            }
```

**Check the actual cmd_search function signature in notes_cmd.rs.** It may need to be adapted.

Mark old `Commands::Search` as `#[arg(hide = true)]` with deprecation passthrough:
```rust
        // Deprecated: use `mur notes search`
        Commands::Search { query, source, result_type, only_sources, only_patterns, limit, json } => {
            eprintln!("# mur search: use `mur notes search`");
            // Forward to notes search with reasonable defaults
            cmd::notes_cmd::cmd_search(&query, limit, json)?
        }
```

- [ ] **Step 10: Internals — add Reindex to InternalsAction**

In `mur-core/src/cli/actions.rs`, add to `InternalsAction`:
```rust
    /// Rebuild the LanceDB vector index from YAML skill files
    Reindex {
        /// Initialise the versioned git store and commit all existing skills
        /// in one bootstrap commit.
        #[arg(long)]
        bootstrap: bool,
    },
```

In dispatch.rs, add to `Commands::Internals { action }` match:
```rust
            InternalsAction::Reindex { bootstrap } => cmd::misc::cmd_reindex(bootstrap)?,
```

**Check:** Find the actual `cmd_reindex` function. Search:
```bash
grep -rn "cmd_reindex\|Reindex" mur-core/src/dispatch.rs | head -5
```

Mark old `Commands::Reindex` as `#[arg(hide = true)]` with deprecation passthrough.

- [ ] **Step 11: Daemon — add dispatch for Commands::Daemon**

In `mur-core/src/dispatch.rs`, add:
```rust
        Commands::Daemon { action } => match action {
            DaemonAction::Start { detach } => cmd::murmurd::cmd_murmurd_start(detach)?,
            DaemonAction::Stop => cmd::murmurd::cmd_murmurd_stop()?,
            DaemonAction::Status => cmd::murmurd::cmd_murmurd_status()?,
            DaemonAction::Serve { port, open, readonly } => {
                cmd::serve_cmd::cmd_serve(port, open, readonly).await?
            }
            DaemonAction::Sleep { action } => match action {
                SleepAction::Enable => cmd::sleep_cmd::cmd_sleep_enable()?,
                SleepAction::Disable => cmd::sleep_cmd::cmd_sleep_disable()?,
                SleepAction::Status => cmd::sleep_cmd::cmd_sleep_status()?,
            },
        },
```

**Check actual function names for serve and sleep.** Search:
```bash
grep -rn "pub.*fn cmd_serve\|pub.*fn.*serve" mur-core/src/cmd/ --include="*.rs" | head -5
grep -rn "pub.*fn.*sleep" mur-core/src/cmd/ --include="*.rs" | head -5
```

Mark old `Commands::Murmurd`, `Commands::Serve`, `Commands::Sleep` as `#[arg(hide = true)]` with deprecation passthrough dispatch.

- [ ] **Step 12: Build and fix all compilation errors**

```bash
cargo build -p mur-core 2>&1
```

Fix any compilation errors — likely issues:
- Missing imports in dispatch.rs
- Mismatched function signatures
- Duplicate variant names in enums

- [ ] **Step 13: Commit**

```bash
git add -A
git commit -m "feat(cli): move commands into noun groups with deprecation passthrough

Session: In/Out/Discard added; Exit/Quit/In/Out hidden
Auth: Login/Logout added as group; standalone hidden
Hook: Inject/Context added; standalone hidden
Workflow: Run/Suggest added; standalone hidden
Chat: merged Conversations + Ask; old variants hidden
Skill: Exchange/Drafts/Eval added; standalone hidden
Notes: Search added; standalone Search hidden
Internals: Reindex added; standalone hidden
Daemon: murmurd/serve/sleep added as group; standalone hidden

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Cross-project reference scan

**Files:** Scanned across `mur` and `mur-commander` repos

- [ ] **Step 1: Scan mur repo for old command references**

```bash
# Search for references to old command names in skills, workflows, scripts, docs
cd /Volumes/Firecuda4tb/Projects/mur
grep -rn "mur exit\|mur quit\|mur in\|mur out\|mur login\|mur logout" --include="*.yaml" --include="*.md" --include="*.sh" --include="*.rs" . | grep -v target/ | grep -v ".git/" | grep -v "docs/superpowers/specs/2026-05-31" | grep -v "docs/superpowers/plans/2026-05-31"
```

```bash
grep -rn "mur search\|mur gc\|mur import\|mur community\|mur suggest\|mur run\|mur murmurd\|mur serve\|mur sleep\|mur conversations\|mur ask\|mur exchange\|mur drafts\|mur eval\|mur reindex\|mur inject\|mur context\|mur push\|mur fetch" --include="*.yaml" --include="*.md" --include="*.rs" . | grep -v target/ | grep -v ".git/" | grep -v "docs/superpowers/specs/2026-05-31" | grep -v "docs/superpowers/plans/2026-05-31"
```

Record all matches. For each, determine if it's:
- **CLI definition** (already handled in Tasks 1-3)
- **Documentation** — update to new command name
- **Skill/workflow file** — update to new command name
- **Script** (install.sh, build.sh, CI) — update to new command name
- **Test** — update to new command name

- [ ] **Step 2: Update all mur doc references**

For each doc file that references an old command, update to the new name. Key files to check:
- `README.md`
- `CLAUDE.md`
- `docs/architecture/runtime-overview.md`
- `docs/superpowers/MIGRATION-STATUS.md`
- Any skill YAML files in `~/.mur/skills/` that may be packaged

- [ ] **Step 3: Update mur scripts and CI**

Check and update:
- `install.sh`
- `build.sh`
- `.github/workflows/*.yml`

- [ ] **Step 4: Scan mur-commander repo**

```bash
cd /Users/david/Projects/mur-commander  # Adjust path as needed
grep -rn "mur in\|mur out\|mur exit\|mur quit\|mur search\|mur run\|mur suggest" --include="*.yaml" --include="*.md" --include="*.rs" .
```

- [ ] **Step 5: Update mur-commander skill references**

For each skill file that invokes old mur commands, update to new names:
- `mur in` → `mur session in`
- `mur out` → `mur session out`
- `mur search` → `mur notes search`
- `mur run` → `mur workflow run`
- `mur suggest` → `mur workflow suggest`

- [ ] **Step 6: Commit (mur repo + mur-commander separately)**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
git add -A
git commit -m "docs: update all references to new CLI command names

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"

cd /Users/david/Projects/mur-commander
git add -A
git commit -m "fix: update mur CLI command references to new names

- mur in → mur session in
- mur out → mur session out
- mur search → mur notes search
- mur run → mur workflow run
- mur suggest → mur workflow suggest

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Update descriptions and internals

**Files:**
- Modify: `mur-core/src/cmd/misc.rs` — doctor, stats
- Modify: `mur-core/src/cli/mod.rs` — doc strings on remaining variants

- [ ] **Step 1: Update `mur doctor` to check skills instead of patterns**

In `mur-core/src/cmd/misc.rs`, find `cmd_doctor`. It currently counts patterns via `YamlStore::default_store()?.list_all()?.len()`. Update to count skills instead.

Read the current implementation first, then update:
```rust
pub(crate) fn cmd_doctor() -> Result<()> {
    let home = dirs::home_dir().expect("no home dir");
    let mur_home = home.join(".mur");

    // 1. Check MUR directory
    if !mur_home.exists() {
        eprintln!("MUR directory not found: {}", mur_home.display());
        eprintln!("Run `mur init` to set up.");
        return Ok(());
    }
    println!("✓ MUR directory: {}", mur_home.display());

    // 2. Count skills (replaces pattern count)
    let skills_dir = mur_home.join("skills");
    let skill_count = if skills_dir.exists() {
        std::fs::read_dir(&skills_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .count()
    } else {
        0
    };
    if skill_count < 5 {
        eprintln!(
            "⚠ Only {} skills found. Run `mur skill install` to add skills.",
            skill_count
        );
    } else {
        println!("✓ Skills: {}", skill_count);
    }

    // 3. Check LLM model (existing logic)
    // ... keep existing model check ...

    Ok(())
}
```

**Note:** Read the actual implementation before modifying. The pattern counting code may be different from what I described.

- [ ] **Step 2: Update `mur stats` to aggregate skill statistics**

In `mur-core/src/cmd/misc.rs`, find `cmd_stats`. It currently analyzes patterns. Update to aggregate skill statistics:
```rust
pub(crate) fn cmd_stats() -> Result<()> {
    let home = dirs::home_dir().expect("no home dir");
    let skills_dir = home.join(".mur").join("skills");

    if !skills_dir.exists() {
        println!("No skills installed yet. Run `mur skill install` to get started.");
        return Ok(());
    }

    let mut total = 0usize;
    let mut by_category = std::collections::HashMap::new();
    let mut by_lifecycle = std::collections::HashMap::new();

    for entry in std::fs::read_dir(&skills_dir)? {
        let entry = entry?;
        if entry.path().is_dir() {
            let manifest_path = entry.path().join("skill.yaml");
            if manifest_path.exists() {
                if let Ok(yaml) = std::fs::read_to_string(&manifest_path) {
                    if let Ok(skill) = serde_yaml::from_str::<mur_common::skill::manifest::SkillManifest>(&yaml) {
                        total += 1;
                        *by_category.entry(format!("{:?}", skill.category)).or_insert(0) += 1;
                        // Lifecycle may be in stats, not manifest — adjust
                    }
                }
            }
        }
    }

    println!("Skills: {}", total);
    println!("By category:");
    for (cat, count) in &by_category {
        println!("  {}: {}", cat, count);
    }

    Ok(())
}
```

**Note:** This is approximate — read the actual `cmd_stats` implementation and adapt to the skill system. The `SkillManifest` / skill stats types may differ from what I'm guessing.

- [ ] **Step 3: Update CLI doc strings that reference "patterns"**

In `mur-core/src/cli/mod.rs`, review all `///` doc comments on remaining Commands variants. Replace "patterns" with "skills" or "notes" as appropriate:

- `/// Sync skills to AI tools` (was "Sync patterns")
- `/// Team shared skills` (was "Team shared patterns")
- `/// Inject skills for a query` (was "Inject patterns")
- `/// Show skill statistics and effectiveness` (was "Show statistics")
- `/// List / show / accept / reject pending skill drafts` (was "pending pattern drafts")

- [ ] **Step 4: Verify `mur notes search` implementation**

Read `mur-core/src/cmd/notes_cmd.rs` — the `cmd_search` function. Confirm it searches notes (category:note skills), not patterns. If it still uses pattern-based scoring, update to skill-based scoring.

- [ ] **Step 5: Build check**

```bash
cargo build -p mur-core 2>&1
cargo clippy -p mur-core -- -D warnings 2>&1 | head -30
```

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor(cli): update doctor/stats to use skills, fix pattern references in docs

- mur doctor: check skill count instead of pattern count
- mur stats: aggregate skill statistics
- Update CLI doc strings: patterns → skills
- mur notes search: verified skill-based search

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: Verify

**Files:** None (verification only)

- [ ] **Step 1: Run the full test suite**

```bash
cargo test --workspace 2>&1
```

Expected: All tests pass. If any test references old command names, fix the test.

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --workspace -- -D warnings 2>&1
```

Expected: No warnings.

- [ ] **Step 3: Run `mur --help` and verify output**

```bash
cargo run -- --help
```

Check:
- [ ] Top-level command count is ~23
- [ ] Old dead commands (gc, import, community) are gone
- [ ] New groups (auth, daemon) appear
- [ ] No hidden variants appear in help
- [ ] Descriptions are updated (no stale "patterns" references)

- [ ] **Step 4: Spot-check new command routing**

```bash
cargo run -- auth login --help
cargo run -- session in --help
cargo run -- session discard --help
cargo run -- daemon start --help
cargo run -- daemon serve --help
cargo run -- daemon sleep --help
cargo run -- hook inject --help
cargo run -- hook context --help
cargo run -- workflow run --help
cargo run -- workflow suggest --help
cargo run -- chat ask --help
cargo run -- chat pull --help
cargo run -- skill exchange --help
cargo run -- skill drafts --help
cargo run -- skill eval --help
cargo run -- notes search --help
cargo run -- internals reindex --help
```

Expected: Each prints valid help text.

- [ ] **Step 5: Spot-check deprecated commands still work**

```bash
cargo run -- exit 2>&1    # Should print deprecation + still work
cargo run -- login 2>&1   # Should print deprecation + still work
cargo run -- search "test" 2>&1  # Should print deprecation + still work
```

- [ ] **Step 6: Commit any final fixes**

```bash
git add -A
git commit -m "chore: final verification fixes for CLI reorganization"
```

---

### Implementation Order

```
Task 1 (Remove dead code)
  → Task 2 (Scaffold new groups)
    → Task 3 (Move commands)
      → Task 4 (Cross-project scan)
        → Task 5 (Update internals)
          → Task 6 (Verify)
```

Each task must complete before starting the next. Tasks 1-3 are the core structural changes. Task 4 catches references outside the main CLI code. Task 5 is the cleanup pass. Task 6 is the gate.
