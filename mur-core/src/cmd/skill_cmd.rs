//! `mur skill` command handlers.

use crate::cmd::agent::resolve_mur_home;
use anyhow::{Context, Result, anyhow, bail};
use mur_common::skill::{
    TrustLevel, local, parse_canonical, parse_legacy_markdown, parse_markdown,
    parser::roundtrip_check, scan::scan_skill, serialize_canonical, serialize_markdown, validate,
};
use std::fs;
use std::path::Path;

/// `mur skill schema [--out path]` — emit the JSON Schema of `SkillManifest`
/// (consumed by the Hub DAG editor for node/edge rendering + per-step forms).
pub fn cmd_schema(out: Option<&str>) -> Result<()> {
    let schema = schemars::schema_for!(mur_common::skill::manifest::SkillManifest);
    let json = serde_json::to_string_pretty(&schema)?;
    match out {
        Some(path) => {
            if let Some(parent) = std::path::Path::new(path).parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, &json)?;
            eprintln!("✓ schema written to {path}");
        }
        None => println!("{json}"),
    }
    Ok(())
}

pub fn cmd_validate(path: &str, warnings_only: bool) -> Result<()> {
    let m = read_any(path)?;
    if let Err(e) = validate(&m) {
        if warnings_only {
            eprintln!("validation: {e}");
        } else {
            bail!("validation failed: {e}");
        }
    }
    let report = scan_skill(&m).context("scan skill")?;
    if report.has_blocking_findings() {
        eprintln!("security findings:");
        for line in report.human_summary() {
            eprintln!("  {line}");
        }
        if !warnings_only {
            bail!("security scan refused the skill");
        }
    }
    // Round-trip integrity: surface silent abstract/context corruption that the
    // markdown↔YAML converter would introduce. This is a warning, not a hard
    // failure — the canonical YAML is unaffected, but `mur skill fmt` would be.
    if let Err(detail) = roundtrip_check(&m) {
        eprintln!("warning: markdown round-trip would alter content (abstract/context)");
        eprintln!("  {}", detail.replace('\n', "\n  "));
    }
    println!("ok: {}", m.name);
    Ok(())
}

pub fn cmd_fmt(path: &str, to: Option<&str>, write: bool) -> Result<()> {
    let p = Path::new(path);
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    let m = read_any(path)?;
    let target = match to {
        Some("yaml") => "yaml",
        Some("md") => "md",
        Some(other) => bail!("unknown target format '{other}' (expected 'yaml' or 'md')"),
        None => {
            if ext == "yaml" {
                "md"
            } else {
                "yaml"
            }
        }
    };
    let out = match target {
        "yaml" => serialize_canonical(&m)?,
        "md" => serialize_markdown(&m)?,
        _ => unreachable!(),
    };
    if write {
        let out_path = p.with_extension(target);
        fs::write(&out_path, out).with_context(|| format!("write {}", out_path.display()))?;
        println!("wrote {}", out_path.display());
    } else {
        print!("{out}");
    }
    Ok(())
}

// --- M1a CRUD + search (Tasks 2-4) ---

pub fn cmd_list() -> Result<()> {
    let home = resolve_mur_home()?;
    let names = local::list_installed(&home).context("list installed skills")?;
    if names.is_empty() {
        println!("(no skills installed)");
        return Ok(());
    }
    for name in &names {
        let level = local::get_trust_level(&home, name).unwrap_or(TrustLevel::Sandboxed);
        // Stay consistent with show/info/audit, which require a loadable manifest:
        // flag directories that `list` would otherwise present as normal skills.
        match local::load_installed(&home, name) {
            Ok(m) => {
                let marker = if m.visibility == mur_common::skill::manifest::Visibility::OnDemand {
                    "  [on-demand]"
                } else {
                    ""
                };
                println!("{name:30} [{level:?}]{marker}");
            }
            Err(_) => {
                println!(
                    "{name:30} [{level:?}]  ⚠ invalid: no readable manifest (run `mur skill remove {name}`)"
                );
            }
        }
    }
    Ok(())
}

pub fn cmd_show(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let m =
        local::load_installed(&home, name).map_err(|_| anyhow!("skill '{name}' not installed"))?;

    // Check mcp_requirements before moving the manifest
    let has_mcp = !m.mcp_requirements.is_empty();

    let yaml = serialize_canonical(&m)?;
    print!("{yaml}");

    if has_mcp {
        println!("\nMCP Requirements:");
        for req in &m.mcp_requirements {
            let cap = &req.capability;
            if req.fallback.is_empty() {
                println!("  - {:<30} (capability: {cap})", req.tool_pattern);
            } else {
                println!(
                    "  - {:<30} (capability: {cap}, fallback: {})",
                    req.tool_pattern, req.fallback
                );
            }
        }
    }

    // Show procedure steps with intent/tool_hint when present.
    if let Some(proc) = &m.content.procedure {
        let has_intent = proc
            .steps
            .iter()
            .any(|s| s.intent.is_some() || s.tool_hint.is_some());
        if has_intent {
            println!("\nProcedure:");
            for (i, step) in proc.steps.iter().enumerate() {
                println!("  {}. {}", i + 1, step.description);
                if let Some(intent) = &step.intent {
                    println!("       intent: {intent}");
                }
                if let Some(hint) = &step.tool_hint {
                    println!("       tool_hint: {hint}");
                }
                match (&step.tool, &step.intent) {
                    (Some(t), Some(_)) => println!("       tool (literal): {t}"),
                    (Some(t), None) => println!("       tool: {t}"),
                    (None, Some(_)) => println!("       (no literal tool)"),
                    (None, None) => {}
                }
            }
        }
    }
    Ok(())
}

/// Pure: apply a scope choice to a manifest. Exactly one of fleet/project/user
/// must be selected; clears the other selectors. Fleet name is slug-validated.
pub(crate) fn set_manifest_scope(
    m: &mut mur_common::skill::manifest::SkillManifest,
    fleet: Option<&str>,
    project: Option<&str>,
    team: Option<&str>,
    user: bool,
) -> Result<()> {
    use mur_common::skill::manifest::SkillScope;
    let n = fleet.is_some() as u8 + project.is_some() as u8 + team.is_some() as u8 + user as u8;
    if n != 1 {
        return Err(anyhow!(
            "specify exactly one of --fleet <name>, --project, --team <id>, or --user"
        ));
    }
    if user {
        m.scope = SkillScope::User;
        m.fleet = None;
        m.project = None;
        m.team = None;
    } else if let Some(f) = fleet {
        if f.trim().is_empty() {
            return Err(anyhow!(
                "--fleet requires a fleet NAME, e.g. --fleet aura-research; the fleet need not exist yet"
            ));
        }
        if !mur_common::fleet::valid_fleet_name(f) {
            return Err(anyhow!(
                "invalid fleet name '{f}': lowercase letters, digits, '-' or '_'"
            ));
        }
        m.scope = SkillScope::Fleet;
        m.fleet = Some(f.to_string());
        m.project = None;
        m.team = None;
    } else if let Some(p) = project {
        m.scope = SkillScope::Project;
        m.project = Some(p.to_string());
        m.fleet = None;
        m.team = None;
    } else if let Some(t) = team {
        if t.trim().is_empty() {
            return Err(anyhow!("--team <id> cannot be empty"));
        }
        m.scope = SkillScope::Team;
        m.team = Some(t.to_string());
        m.fleet = None;
        m.project = None;
    }
    Ok(())
}

/// `mur skill scope <name>` — set a skill's visibility scope so the scope-aware
/// injector (CLI + runtime) only surfaces it in the matching context. `--project`
/// scopes to the current git repo; `--fleet <name>` to a fleet; `--team <id>` to a team; `--user` resets.
pub fn cmd_scope(
    name: &str,
    fleet: Option<String>,
    project: bool,
    team: Option<String>,
    user: bool,
) -> Result<()> {
    let home = resolve_mur_home()?;
    let dir = local::installed_path(&home, name);
    let mut m =
        local::load_installed(&home, name).map_err(|_| anyhow!("skill '{name}' not installed"))?;
    let proj_id = if project {
        let cwd = std::env::current_dir()?;
        Some(
            mur_common::project::project_id(&cwd)
                .ok_or_else(|| anyhow!("--project requires being inside a git repo"))?,
        )
    } else {
        None
    };
    set_manifest_scope(
        &mut m,
        fleet.as_deref(),
        proj_id.as_deref(),
        team.as_deref(),
        user,
    )?;
    m.updated_at = chrono::Utc::now();
    mur_common::skill::store::write_to_dir(&dir, &m)
        .map_err(|e| anyhow!("write skill '{name}': {e}"))?;
    println!("Set '{name}' scope = {:?}", m.scope);
    Ok(())
}

pub fn cmd_remove(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    local::remove_installed(&home, name).map_err(|e| anyhow!("failed to remove '{name}': {e}"))?;

    // Best-effort: remove the skill's embedding from LanceDB.
    //
    // Driven on an isolated runtime: `mur` runs its whole CLI inside a tokio
    // runtime, so `Handle::current().block_on()` here panicked with "Cannot
    // start a runtime from within a runtime" — AFTER the files were already
    // deleted, so the removal looked like a crash and printed no confirmation.
    let _ = crate::cmd::skill_install::block_on_isolated(|| async {
        let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
        let index_dir = home.join("lance");
        if let Ok(store) = crate::store::vector::factory::get_vector_store(&cfg, &index_dir).await {
            let _ = crate::skill_index::delete(name, &*store).await;
        }
    });

    println!("removed: {name}");
    Ok(())
}

pub fn cmd_search(query: &str, local_only: bool) -> Result<()> {
    let home = resolve_mur_home()?;
    let local_results = local::search_installed(&home, query).context("search installed")?;

    if !local_results.is_empty() {
        println!("Installed:");
        for (name, m) in &local_results {
            let level = local::get_trust_level(&home, name)
                .unwrap_or(mur_common::skill::TrustLevel::Sandboxed);
            println!("  {name:25} {:12?} {}", level, m.description);
        }
    }

    if !local_only {
        match crate::cmd::skill_registry::fetch_and_load(
            &home,
            crate::cmd::skill_registry::DEFAULT_REGISTRY,
        ) {
            Ok((_dir, idx)) => {
                let reg_results = crate::cmd::skill_registry::search_registry(&idx, query);
                if !reg_results.is_empty() {
                    if !local_results.is_empty() {
                        println!();
                    }
                    println!("Registry:");
                    for (name, entry) in reg_results {
                        println!(
                            "  {name:25} registry    {} [v{}]",
                            entry.description, entry.latest
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("warning: registry search failed: {e}");
            }
        }
    }

    if local_results.is_empty() && local_only {
        println!("(no matching installed skills found)");
    }

    Ok(())
}

// --- M1b audit + trust (Tasks 5-6) ---

pub fn cmd_info(name: &str, full: bool, metrics: bool) -> Result<()> {
    let home = resolve_mur_home()?;
    let m =
        local::load_installed(&home, name).map_err(|_| anyhow!("skill '{name}' not installed"))?;
    let level =
        local::get_trust_level(&home, name).unwrap_or(mur_common::skill::TrustLevel::Sandboxed);
    println!("Name:        {}", m.name);
    println!("Version:     {}", m.version);
    println!("Publisher:   {}", m.publisher);
    println!("Description: {}", m.description);
    println!("Category:    {:?}", m.category);
    println!("Tags:        {}", m.tags.join(", "));
    println!("Trust Level: {level:?}");
    if full {
        println!("\n--- Abstract ---\n{}", m.content.r#abstract);
    }
    if metrics {
        print_metrics(&home, name)?;
    }
    Ok(())
}

fn print_metrics(home: &Path, name: &str) -> Result<()> {
    use mur_common::skill::lifecycle::next_state;
    use mur_common::skill::stats::SkillStats;

    let stats_path = SkillStats::path(home, name);
    let stats = match SkillStats::load(&stats_path)? {
        Some(s) => s,
        None => {
            println!("\nMetrics: (no stats — run `mur skill reindex-stats {name}`)");
            return Ok(());
        }
    };

    let now = chrono::Utc::now();
    let proposed = next_state(
        &stats,
        now,
        &mur_common::skill::lifecycle::LifecycleThresholds::default(),
    );

    let success_rate = if stats.usage_count == 0 {
        0.0
    } else {
        stats.success_count as f64 / stats.usage_count as f64 * 100.0
    };

    let half_life = mur_common::skill::lifecycle::half_life_days(stats.lifecycle_state);
    let decayed = mur_common::skill::lifecycle::calculate_decay(
        stats.anchor_confidence,
        stats.last_success_at,
        half_life,
        now,
    );

    let last_used_str = stats
        .last_used_at
        .map(|t| {
            let days = (now - t).num_days();
            format!("{} ({} days ago)", t.format("%Y-%m-%d"), days)
        })
        .unwrap_or_else(|| "never".to_string());

    let first_ok_str = stats
        .first_successful_use_at
        .map(|t| {
            let days = (now - t).num_days();
            format!("{} ({} days ago)", t.format("%Y-%m-%d"), days)
        })
        .unwrap_or_else(|| "never".to_string());

    let proposed_note = if proposed != stats.lifecycle_state {
        format!(" — proposed: {proposed:?} (promotion eligible after sweep)")
    } else {
        String::new()
    };

    println!("\nMetrics:");
    println!(
        "  state:       {:?}{}",
        stats.lifecycle_state, proposed_note
    );
    println!("  pinned:      {}", if stats.pinned { "yes" } else { "no" });
    println!(
        "  usage:       {} (success {} / failure {} / rate {:.0}%)",
        stats.usage_count, stats.success_count, stats.failure_count, success_rate
    );
    println!(
        "  confidence:  {:.2}  (anchor {:.2}, decayed over {:.0}d half-life)",
        decayed, stats.anchor_confidence, half_life
    );
    println!("  last used:   {last_used_str}");
    println!("  first ok:    {first_ok_str}");

    Ok(())
}

pub fn cmd_audit(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let m =
        local::load_installed(&home, name).map_err(|_| anyhow!("skill '{name}' not installed"))?;

    let report = scan_skill(&m)?;
    if report.has_blocking_findings() {
        eprintln!("Security findings:");
        for line in report.human_summary() {
            eprintln!("  {line}");
        }
    } else {
        println!("Content scan: clean");
    }

    // Trust-store key: must match what the loader looks up.
    let hash = mur_common::skill::content_hash_for_trust(&m)?;
    let trust = mur_common::trust::skills::SkillTrustStore::load(&home)
        .map_err(|e| anyhow!("load trust store: {e}"))?;
    match trust.lookup(&hash) {
        Some(e) => println!(
            "Trust: {:?} (publisher: {})",
            e.level,
            e.publisher.as_deref().unwrap_or("-")
        ),
        None => println!("No trust entry (defaults to Sandboxed)"),
    }

    println!("Audit complete for '{name}'");
    Ok(())
}

pub fn cmd_trust(name: &str, level_str: &str) -> Result<()> {
    let level = match level_str {
        "sandboxed" => mur_common::skill::TrustLevel::Sandboxed,
        "verified" => mur_common::skill::TrustLevel::Verified,
        "trusted" => mur_common::skill::TrustLevel::Trusted,
        other => bail!("invalid level '{other}' (expected: sandboxed | verified | trusted)"),
    };
    let home = resolve_mur_home()?;
    let m =
        local::load_installed(&home, name).map_err(|_| anyhow!("skill '{name}' not installed"))?;
    // Trust-store key: must match what the loader looks up.
    let hash = mur_common::skill::content_hash_for_trust(&m)?;

    let mut trust = mur_common::trust::skills::SkillTrustStore::load(&home)
        .map_err(|e| anyhow!("load trust store: {e}"))?;
    trust.insert(
        hash,
        mur_common::trust::skills::TrustEntry {
            name: name.to_string(),
            version: m.version.clone(),
            level,
            installed_at: chrono::Utc::now().to_rfc3339(),
            publisher: Some(m.publisher.clone()),
            ..Default::default()
        },
    );
    trust
        .save(&home)
        .map_err(|e| anyhow!("save trust store: {e}"))?;
    println!("Trust level for '{name}' set to {level:?}");
    Ok(())
}

/// Options for `mur skill new`.
pub struct NewOptions {
    pub name: String,
    pub category: String,
    /// Write under this directory instead of the current dir.
    pub dir: Option<String>,
    /// Install into the agent's loadable layout under `<MUR_HOME>`.
    pub agent: Option<String>,
    pub force: bool,
}

/// Resolve the per-skill subdirectory (`.../<name>/`) for a given target.
///
/// Resolution order (matches `cmd_new` and `cmd_edit`):
/// * `--agent <a>` → `<MUR_HOME>/agents/<a>/skills/<name>` (the loadable layout)
/// * `--dir <d>`   → `<d>/<name>`
/// * neither       → `./<name>` (current dir)
fn resolve_skill_subdir(
    name: &str,
    agent: Option<&str>,
    dir: Option<&str>,
) -> Result<std::path::PathBuf> {
    // Path-safety gate: the name becomes a path component, so reject anything
    // that could escape the target dir (`..`, `/`, etc). Reused from the loader.
    if !mur_common::skill::is_valid_skill_name(name) {
        bail!("invalid skill name {name:?} (expected [A-Za-z0-9_.-], no path separators)");
    }
    let base = if let Some(agent) = agent {
        mur_common::skill::agent_skill_dir(&resolve_mur_home()?, agent)
    } else if let Some(dir) = dir {
        std::path::PathBuf::from(dir)
    } else {
        std::path::PathBuf::from(".")
    };
    Ok(base.join(name))
}

/// Map a `--category` string to a canonical `Category`, restricted to the
/// categories that make sense for hand-authoring a `context`-bodied scaffold.
fn parse_authoring_category(s: &str) -> Result<mur_common::skill::Category> {
    use mur_common::skill::Category;
    match s {
        "context" => Ok(Category::Context),
        "workflow" => Ok(Category::Workflow),
        "command" => Ok(Category::Command),
        "meta" => Ok(Category::Meta),
        "note" => Ok(Category::Note),
        other => bail!(
            "invalid category {other:?} (expected one of: context, workflow, command, meta, note)"
        ),
    }
}

/// Current user for the `publisher: human:<user>` default, falling back to
/// `you` so the generated manifest is valid even without `$USER`.
fn current_user() -> String {
    std::env::var("USER")
        .ok()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| "you".to_string())
}

/// Render the templated `skill.yaml` body. Hand-written (not serde-serialized)
/// so it can carry inline `#` comments guiding the author through the
/// abstract-vs-context progressive-disclosure split.
fn render_skill_template(name: &str, category: mur_common::skill::Category) -> String {
    use mur_common::skill::Category;
    let publisher = format!("human:{}", current_user());
    // The body shape differs per category: context/meta/note carry prose; only
    // `context`/`meta` use a `context:` block. To keep the scaffold always-valid
    // and focused on the authoring guidance the task calls for, we template the
    // `context` body (the common case) and adapt the content block per category.
    let content_block = match category {
        Category::Workflow => {
            "  # `procedure` holds the executable steps for a workflow skill.\n  procedure:\n    steps:\n      - description: TODO describe the first step\n"
        }
        Category::Command => {
            "  # `command` is the shell/command body run for a command skill.\n  command: \"echo TODO: implement this command\"\n"
        }
        Category::Note => {
            "  # `note` is a free-form markdown body (category: note).\n  note: |\n    # TODO: reference notes\n    - jot down durable facts here\n"
        }
        // context + meta both use a `context` block scalar.
        _ => {
            "  # `context` is the FULL reference body. It is loaded ONLY when a\n  # trigger above fires (progressive disclosure) — so it can be long.\n  # The `|` makes this a LITERAL block scalar: newlines and indentation are\n  # preserved verbatim, so write normal markdown here.\n  context: |\n    # TODO: full reference body for this skill\n    - Replace this with the rules, steps, and details the agent needs\n      once the skill is triggered.\n    - Everything here is hidden until a trigger fires, so be thorough.\n"
        }
    };
    format!(
        "# Scaffolded by `mur skill new`. Edit the TODOs, then run `mur skill validate`.\n\
         name: {name}\n\
         version: 1.0.0\n\
         publisher: {publisher}\n\
         # One-line trigger: what it is + WHEN to use it + searchable keywords.\n\
         description: \"TODO: one-line trigger — what this is and when to use it.\"\n\
         category: {category}\n\
         priority: normal\n\
         tags: [todo]\n\
         triggers:\n\
         \x20\x20# Always-on: the abstract below is injected at session start.\n\
         \x20\x20- type: session_start\n\
         \x20\x20# Keyword: load the full context when these words appear. Edit the regex.\n\
         \x20\x20- type: keyword\n\
         \x20\x20\x20\x20pattern: '(?i)\\b({name}|todo-keyword)\\b'\n\
         \x20\x20# Command: invoke explicitly with /{name}.\n\
         \x20\x20- type: command\n\
         \x20\x20\x20\x20pattern: /{name}\n\
         content:\n\
         \x20\x20# `abstract` is ALWAYS-ON: injected into the system prompt every turn.\n\
         \x20\x20# Keep it to 1-3 tight sentences — it costs tokens on every message.\n\
         \x20\x20abstract: >\n\
         \x20\x20\x20\x20TODO: 1-3 sentence always-on summary of this skill.\n\
         {content_block}",
        category = category_yaml_value(category),
    )
}

/// The lowercase YAML scalar for a category (matches serde's kebab/lowercase
/// serialization used elsewhere in the manifest).
fn category_yaml_value(category: mur_common::skill::Category) -> &'static str {
    use mur_common::skill::Category;
    match category {
        Category::Context => "context",
        Category::Workflow => "workflow",
        Category::Command => "command",
        Category::Meta => "meta",
        Category::Note => "note",
        Category::Media => "media",
    }
}

/// Pure helper: when scaffolding a new skill with neither `--agent` nor
/// `--dir`, default the output root to `<mur_home>/skills` instead of the
/// current directory (`resolve_skill_subdir`'s CWD fallback, which `cmd_edit`
/// still relies on and must keep). Returns `dir` unchanged in all other cases.
fn new_skill_dir(mur_home: &Path, agent: Option<&str>, dir: Option<&str>) -> Option<String> {
    if agent.is_none() && dir.is_none() {
        Some(mur_home.join("skills").to_string_lossy().into_owned())
    } else {
        dir.map(str::to_string)
    }
}

/// Core scaffold logic for `mur skill new` — returns the written path.
/// Split out from `cmd_new` so it can be unit-tested without going through CLI
/// dispatch or touching the real `$MUR_HOME`.
fn scaffold_skill(opts: NewOptions) -> Result<std::path::PathBuf> {
    let category = parse_authoring_category(&opts.category)?;
    let mur_home = resolve_mur_home()?;
    let dir = new_skill_dir(&mur_home, opts.agent.as_deref(), opts.dir.as_deref());
    let subdir = resolve_skill_subdir(&opts.name, opts.agent.as_deref(), dir.as_deref())?;
    let target = subdir.join("skill.yaml");

    if target.exists() && !opts.force {
        bail!(
            "{} already exists (use --force to overwrite)",
            target.display()
        );
    }

    let body = render_skill_template(&opts.name, category);

    // Fail BEFORE writing if the assembled manifest would not parse/validate.
    // This catches names that pass the path-safety gate but violate the stricter
    // manifest rules (e.g. uppercase or `_`, which `validate` rejects).
    let parsed = parse_canonical(&body).context("template did not parse as a skill manifest")?;
    validate(&parsed).context("scaffolded manifest failed validation")?;

    fs::create_dir_all(&subdir)
        .with_context(|| format!("create skill dir {}", subdir.display()))?;
    fs::write(&target, &body).with_context(|| format!("write {}", target.display()))?;
    Ok(target)
}

/// `mur skill new <name>` — scaffold a new skill manifest.
pub fn cmd_new(opts: NewOptions) -> Result<()> {
    let name = opts.name.clone();
    let target = scaffold_skill(opts)?;
    println!("✓ scaffolded skill '{name}' at {}", target.display());
    println!(
        "  edit the TODOs, then: mur skill validate {}",
        target.display()
    );
    Ok(())
}

/// Outcome of a post-edit validation pass.
#[derive(Debug)]
struct EditReport {
    valid: bool,
}

/// Core `edit` logic with an injectable editor invocation, so tests can drive
/// it without a real interactive `$EDITOR`. `open` is called with the resolved
/// `skill.yaml` path; after it returns, the manifest is re-validated.
fn run_edit<F>(name: &str, agent: Option<&str>, dir: Option<&str>, open: F) -> Result<EditReport>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let subdir = resolve_skill_subdir(name, agent, dir)?;
    let target = subdir.join("skill.yaml");
    if !target.exists() {
        bail!(
            "no skill.yaml at {} — create one first with `mur skill new {name}`",
            target.display()
        );
    }

    open(&target).with_context(|| format!("editing {}", target.display()))?;

    // Re-validate after the editor exits. We report (rather than bail) so the
    // author sees the result and can re-edit; only I/O/parse failures bubble up.
    match read_any(target.to_str().unwrap_or_default()) {
        Ok(m) => match validate(&m) {
            Ok(()) => {
                println!("ok: {}", m.name);
                Ok(EditReport { valid: true })
            }
            Err(e) => {
                eprintln!("validation: {e}");
                Ok(EditReport { valid: false })
            }
        },
        Err(e) => {
            eprintln!("parse error: {e}");
            Ok(EditReport { valid: false })
        }
    }
}

/// `mur skill edit <name>` — open the resolved skill.yaml in `$EDITOR`
/// (fallback `$VISUAL`, then `vi`), then validate.
pub fn cmd_edit(name: &str, agent: Option<&str>, dir: Option<&str>) -> Result<()> {
    let report = run_edit(name, agent, dir, |path| {
        let editor = std::env::var("EDITOR")
            .ok()
            .filter(|e| !e.trim().is_empty())
            .or_else(|| {
                std::env::var("VISUAL")
                    .ok()
                    .filter(|e| !e.trim().is_empty())
            })
            .unwrap_or_else(|| "vi".to_string());
        let status = std::process::Command::new(&editor)
            .arg(path)
            .status()
            .with_context(|| format!("failed to launch editor {editor:?}"))?;
        if !status.success() {
            bail!("editor {editor:?} exited with status {status}");
        }
        Ok(())
    })?;
    if !report.valid {
        bail!("skill did not pass validation after edit");
    }
    Ok(())
}

fn read_any(path: &str) -> Result<mur_common::skill::SkillManifest> {
    let text = fs::read_to_string(path).with_context(|| format!("read {path}"))?;
    let p = Path::new(path);
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    let m = if ext == "yaml" || ext == "yml" {
        parse_canonical(&text)?
    } else if text.contains("\n---") || text.starts_with("---") {
        match parse_markdown(&text) {
            Ok(m) => m,
            Err(_) => parse_legacy_markdown(&text)?,
        }
    } else {
        bail!("cannot detect skill format for {path}");
    };
    Ok(m)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod remove_tests;
