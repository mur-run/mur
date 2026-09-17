//! `mur monitor` — the user's first diagnostic surface (spec §CLI, §可觀測性).
//! Every verb is `run_to` over an explicit writer and clock so the tests
//! read what a user reads.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::Subcommand;
use mur_common::hitl::RiskTier;
use mur_common::secret::SecretRef;
use mur_monitor::action::ActionState;
use mur_monitor::spec::MonitorSpec;
use mur_monitor::state::MonitorState;
use mur_monitor::store::{ListFilter, MonitorRow, MonitorStore};

/// Observations shown by `show` without `--history`.
const SHOW_RECENT_OBSERVATIONS: usize = 5;
/// Characters of the id `list`/`show` display and `resolve_id` accepts as a
/// prefix. Ids are UUIDv7 (`xxxxxxxx-xxxx-Vxxx-...`): the first 13 characters
/// (8 hex + `-` + 4 hex) are exactly the 48-bit millisecond timestamp, so any
/// two monitors created in *different* milliseconds are always distinct at
/// this length — 8 was too short: it only covers the top 32 of those 48
/// bits, so any two monitors created within the same ~65-second window (2^16
/// ms) shared it. Two monitors created in the very same millisecond still
/// collide even at 13 (the random bits start after the version nibble at
/// index 14) — that residual case is exactly what `resolve_id`'s ambiguous
/// match error is for.
const ID_SHORT: usize = 13;

#[derive(Debug, Subcommand)]
pub enum MonitorAction {
    /// Validate a spec file, probe the source once, and register the monitor.
    Add {
        /// Path to a MonitorSpec YAML file.
        #[arg(long)]
        file: PathBuf,
        /// When the monitored work really started (RFC 3339). Deadlines count from here. Default: now.
        #[arg(long)]
        started_at: Option<String>,
    },
    /// List monitors that still need attention (add --all for completed ones).
    List {
        /// Only this state (active, sleeping, checking, action_pending, awaiting_approval, completed, exhausted).
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// One monitor in detail: spec, lease, recent evidence.
    Show {
        id: String,
        /// Also print the full append-only event history.
        #[arg(long)]
        history: bool,
    },
    /// Stop monitoring. Does NOT cancel the monitored work.
    Cancel { id: String },
    /// Bring an exhausted monitor back to active.
    Retry {
        id: String,
        /// Also reset the automatic remediation counter to zero.
        #[arg(long)]
        reset_remediation_budget: bool,
    },
}

pub fn run(mur_home: &Path, action: MonitorAction) -> Result<()> {
    run_to(mur_home, action, &mut std::io::stdout(), Utc::now())
}

pub fn run_to(
    mur_home: &Path,
    action: MonitorAction,
    out: &mut dyn Write,
    now: DateTime<Utc>,
) -> Result<()> {
    let store = MonitorStore::open(mur_home)?;
    match action {
        MonitorAction::Add { file, started_at } => {
            add(mur_home, &store, &file, started_at.as_deref(), out, now)
        }
        MonitorAction::List { state, all } => list(&store, state.as_deref(), all, out, now),
        MonitorAction::Show { id, history } => show(&store, &id, history, out),
        MonitorAction::Cancel { id } => cancel(&store, &id, out, now),
        MonitorAction::Retry {
            id,
            reset_remediation_budget,
        } => retry(&store, &id, reset_remediation_budget, out, now),
    }
}

fn add(
    mur_home: &Path,
    store: &MonitorStore,
    file: &Path,
    started_at: Option<&str>,
    out: &mut dyn Write,
    now: DateTime<Utc>,
) -> Result<()> {
    warn_on_mur_home_divergence(mur_home, out)?;
    let yaml = std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let spec = MonitorSpec::from_yaml(&yaml)?;
    spec.validate()?;
    let registry = crate::monitor::registry(mur_home);
    let Some(adapter) = registry.get(spec.source.r#type) else {
        let enabled: Vec<_> = registry.types().iter().map(|t| t.as_str()).collect();
        bail!(
            "no adapter enabled for source type `{}` (enabled: {})",
            spec.source.r#type.as_str(),
            enabled.join(", ")
        );
    };
    adapter
        .validate_reference(&spec.source.reference)
        .map_err(|e| anyhow::anyhow!("source.reference `{}`: {e}", spec.source.reference))?;
    // Rule 3: one read-only query now. A permission problem must be said
    // out loud instead of becoming a monitor that can never answer.
    let probe = adapter.observe(
        &spec.source.reference,
        spec.source.credential_ref.as_deref(),
    );
    if probe.credential_failure {
        let err = probe
            .adapter_error
            .as_deref()
            .unwrap_or("credential failure");
        bail!(
            "{err} — fix the credential reference before adding; a monitor that can never query is not created"
        );
    }
    // Rule 3 applied to the write grant (F2, whole-branch review). The read
    // credential one field over gets a live probe; this one gets a resolve
    // and nothing more — proving it grants *write* scope would mean
    // performing an actual write, which nobody has approved at `add` time.
    // Resolving catches the typo and the missing key, which is the failure
    // that otherwise surfaces only AFTER a human approves the rerun:
    // approving something that was never going to run is worse than a clear
    // refusal.
    //
    // Warn, never bail. `resolve_to_string_blocking` answers `None` for "no
    // such key" and for "the backend was unavailable" alike (a locked
    // keychain, a headless box), so a refusal here would reject valid specs
    // for something the user cannot fix at that moment. Echoing the
    // reference is safe: `spec.validate()` above already refused anything
    // that is not a parseable `SecretRef`, so what is printed is provably a
    // reference and not a value.
    if let Some(grant) = &spec.source.write_credential_ref
        && SecretRef::from_str(grant)
            .ok()
            .and_then(|r| r.resolve_to_string_blocking())
            .is_none()
    {
        writeln!(
            out,
            "warning: source.write_credential_ref `{grant}` does not resolve on this \
             machine — an approved remedy would fail when it runs. Set the secret, or \
             fix the reference and re-add."
        )?;
    }
    let started = match started_at {
        Some(s) => Some(
            DateTime::parse_from_rfc3339(s)
                .context("--started-at must be RFC 3339")?
                .with_timezone(&Utc),
        ),
        None => None,
    };
    let c = store.create(&spec, now, started)?;
    if c.existing {
        writeln!(
            out,
            "monitor {} already exists for idempotency_key {}",
            c.id, spec.idempotency_key
        )?;
    } else {
        writeln!(
            out,
            "monitor {} · {} {} · first check {} · probe: {}",
            c.id,
            spec.source.r#type.as_str(),
            spec.source.reference,
            c.next_check_at.to_rfc3339(),
            probe.outcome.as_str()
        )?;
    }
    Ok(())
}

/// `mur monitor add` (CLI/murmur) resolves its home via `crate::paths::mur_root`,
/// which honors `MUR_HOME`. The daemon — which is what actually polls this
/// monitor every `TICK_INTERVAL` — resolves its home via
/// `crate::store::yaml::default_mur_dir()`, which does NOT (see that
/// function's doc comment; changing daemon path resolution is out of scope
/// here, it is a repo-wide condition affecting every daemon subsystem, not
/// just monitors). With `MUR_HOME` set to something other than the default,
/// a monitor created here would silently be written under a directory the
/// daemon never looks at and would just never advance — say so instead of
/// letting the user discover it by staring at a monitor stuck at `sleeping`
/// forever.
fn warn_on_mur_home_divergence(mur_home: &Path, out: &mut dyn Write) -> Result<()> {
    let Ok(configured) = std::env::var("MUR_HOME") else {
        return Ok(());
    };
    if configured.is_empty() {
        return Ok(());
    }
    let daemon_home = crate::store::yaml::default_mur_dir();
    if mur_home != daemon_home {
        writeln!(
            out,
            "warning: MUR_HOME={configured} makes `mur monitor add` use {} — the daemon \
             polls monitors under {} and will never see this one",
            mur_home.display(),
            daemon_home.display()
        )?;
    }
    Ok(())
}

fn hard_deadline_at(r: &MonitorRow) -> DateTime<Utc> {
    r.work_started_at
        + chrono::Duration::from_std(r.spec.policy.hard_deadline()).unwrap_or(chrono::Duration::MAX)
}

fn list(
    store: &MonitorStore,
    state: Option<&str>,
    all: bool,
    out: &mut dyn Write,
    now: DateTime<Utc>,
) -> Result<()> {
    let state = match state {
        None => None,
        Some(s) => Some(MonitorState::parse(s).ok_or_else(|| {
            let valid: Vec<_> = MonitorState::ALL.iter().map(|v| v.as_str()).collect();
            anyhow::anyhow!("unknown state `{s}` (valid: {})", valid.join(", "))
        })?),
    };
    let rows = store.list(&ListFilter {
        state,
        include_completed: all,
    })?;
    render_list(&rows, out, now)
}

/// The `mur monitor list` row rendering, factored out so the murmur TUI's
/// `/monitor` (and `Ctrl+T`) can print the same card into the scrollback
/// instead of maintaining a second renderer.
pub(crate) fn render_list(
    rows: &[MonitorRow],
    out: &mut dyn Write,
    now: DateTime<Utc>,
) -> Result<()> {
    if rows.is_empty() {
        writeln!(out, "no monitors")?;
        return Ok(());
    }
    writeln!(
        out,
        "{:<id_w$}  {:<20}  {:<17}  {:<9}  {:>10}  {:>10}  HARD DEADLINE",
        "ID",
        "NAME",
        "STATE",
        "OUTCOME",
        "PROGRESS",
        "NEXT",
        id_w = ID_SHORT
    )?;
    for r in rows {
        writeln!(
            out,
            "{:<id_w$}  {:<20}  {:<17}  {:<9}  {:>10}  {:>10}  {}",
            &r.id[..ID_SHORT.min(r.id.len())],
            truncate(&r.name, 20),
            r.state.as_str(),
            r.outcome.as_str(),
            ago(now, r.last_progress_at),
            until(now, r.next_check_at),
            hard_deadline_at(r).to_rfc3339(),
            id_w = ID_SHORT
        )?;
    }
    Ok(())
}

/// Resolve a user-typed id the way `git` resolves a short SHA: an exact id
/// always wins immediately, otherwise the argument is a prefix candidate.
/// `list` only ever shows the first `ID_SHORT` characters, so `show` /
/// `cancel` / `retry` must accept exactly what `list` printed — searching
/// completed monitors too, since `show` must still work on finished work
/// that `list`'s default filter hides. Returns the resolved row itself (not
/// just the id) so callers don't repeat the lookup they just did to resolve it.
fn resolve_id(store: &MonitorStore, id: &str) -> Result<MonitorRow> {
    if let Some(r) = store.get(id)? {
        return Ok(r);
    }
    let all = store.list(&ListFilter {
        state: None,
        include_completed: true,
    })?;
    let mut matches: Vec<MonitorRow> = all.into_iter().filter(|r| r.id.starts_with(id)).collect();
    match matches.len() {
        0 => bail!("no monitor `{id}`"),
        1 => Ok(matches.remove(0)),
        n => {
            let candidates: Vec<String> = matches
                .iter()
                .map(|r| format!("{} ({})", &r.id[..ID_SHORT.min(r.id.len())], r.name))
                .collect();
            bail!(
                "ambiguous id `{id}` matches {n} monitors: {} — give more characters",
                candidates.join(", ")
            );
        }
    }
}

fn show(store: &MonitorStore, id: &str, history: bool, out: &mut dyn Write) -> Result<()> {
    let r = resolve_id(store, id)?;
    let id = r.id.as_str();
    writeln!(out, "monitor {} ({})", r.id, r.name)?;
    writeln!(
        out,
        "  source:        {} {}",
        r.source_type.as_str(),
        r.reference
    )?;
    if let Some(c) = &r.spec.source.credential_ref {
        writeln!(out, "  credential:    {c} (reference only)")?;
    }
    writeln!(
        out,
        "  state/outcome: {} / {}",
        r.state.as_str(),
        r.outcome.as_str()
    )?;
    writeln!(out, "  work started:  {}", r.work_started_at.to_rfc3339())?;
    writeln!(
        out,
        "  deadlines:     stalled {} · soft {} · hard {}",
        r.spec.policy.stalled_after, r.spec.policy.soft_deadline, r.spec.policy.hard_deadline
    )?;
    writeln!(
        out,
        "  last progress: {}  token: {}",
        r.last_progress_at.to_rfc3339(),
        r.progress_token.as_deref().unwrap_or("-")
    )?;
    writeln!(
        out,
        "  next check:    {}  (pending attempts {}, unknown streak {})",
        r.next_check_at.to_rfc3339(),
        r.pending_attempts,
        r.unknown_streak
    )?;
    // `footer::has_condition` counts exactly these three fields toward the
    // murmur `monitor(n)` badge — `show` must surface them too, or a user
    // staring at one monitor has no way to see why the footer is lit.
    writeln!(
        out,
        "  condition:     stalled since {} · soft notified {} · hard reached {}",
        r.stalled_since
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
        r.soft_notified,
        r.hard_reached
    )?;
    match store.lease_of(id)? {
        Some(l) => writeln!(
            out,
            "  lease:         {} until {} (fence {} — guards against a reclaimed worker overwriting a newer cycle)",
            l.owner,
            l.expires_at.to_rfc3339(),
            l.fence
        )?,
        None => writeln!(
            out,
            "  lease:         none (fence {} — guards against a reclaimed worker overwriting a newer cycle)",
            r.fence
        )?,
    }
    let deliveries = store.delivery_states(id)?;
    if !deliveries.is_empty() {
        writeln!(out, "  notifications:")?;
        for (event_id, channel, state, attempts) in deliveries {
            writeln!(
                out,
                "    event {event_id}  {channel:<8} {}{}",
                state.as_str(),
                if attempts > 0 {
                    format!(" ({attempts} attempt(s))")
                } else {
                    String::new()
                }
            )?;
        }
    }
    let actions = store.actions_for(id)?;
    if !actions.is_empty() {
        writeln!(out, "  actions:")?;
        for a in &actions {
            // Single parser for the (positional, colon-delimited) action-key
            // format — `crate::monitor::drain_actions::verb_and_index_from_key`
            // is the same recovery Phase 2's retry path uses; a second,
            // hand-rolled copy here would be how a later change to the key
            // silently breaks only one caller. Falls back to the raw key
            // rather than failing `show` on a row it cannot fully explain.
            let verb = crate::monitor::drain_actions::verb_and_index_from_key(&a.action_key)
                .map_or(a.action_key.as_str(), |(v, _)| v);
            let attempt = if a.attempt > 0 {
                format!(
                    " ({} attempt{})",
                    a.attempt,
                    if a.attempt == 1 { "" } else { "s" }
                )
            } else {
                String::new()
            };
            // Rule 1: a blocked action that does not also print the exact
            // command that releases it is, from the user's side, a monitor
            // that silently stopped — this line is the whole payoff of the
            // slice, not a nice-to-have.
            let unblock = match (a.state, &a.approval_id) {
                (ActionState::Blocked, Some(hitl_id)) => format!(
                    "  → mur channel approve {} {hitl_id}",
                    crate::monitor::actions::gate::channel_id_for(&a.monitor_id)
                ),
                _ => String::new(),
            };
            writeln!(
                out,
                "    {:<13} {:<6} {}{attempt}{unblock}",
                verb,
                risk_str(a.risk),
                a.state.as_str(),
            )?;
            // The stored `result` — the reason a remedy failed, or what a
            // `collect_logs` collected. `remediation_failed`'s notification
            // sends the user here ("`mur monitor show <id>` for what was
            // tried") and until now the only place this column was visible
            // was the raw event payload behind `--history`. It is already
            // redacted and length-capped by `store_result`, so it is safe to
            // print as-is; its own line, because it is prose and the columns
            // above are not.
            if let Some(result) = a.result.as_deref().map(str::trim)
                && !result.is_empty()
            {
                writeln!(out, "      {result}")?;
            }
        }
    }
    writeln!(out, "  recent observations:")?;
    for o in store.observations(id, SHOW_RECENT_OBSERVATIONS)? {
        writeln!(
            out,
            "    {}  {:<9}  {}{}",
            o.observed_at.to_rfc3339(),
            o.outcome.as_str(),
            o.evidence,
            o.adapter_error
                .map(|e| format!("  [{e}]"))
                .unwrap_or_default()
        )?;
    }
    if history {
        writeln!(out, "  history:")?;
        for e in store.events(id)? {
            writeln!(
                out,
                "    {}  {:<18}  {}",
                e.created_at.to_rfc3339(),
                e.kind,
                e.payload
            )?;
        }
    }
    Ok(())
}

fn cancel(store: &MonitorStore, id: &str, out: &mut dyn Write, now: DateTime<Utc>) -> Result<()> {
    let r = resolve_id(store, id)?;
    let id = r.id.as_str();
    if r.state == MonitorState::Completed {
        bail!("monitor {id} is already completed");
    }
    store.set_state(id, MonitorState::Completed, now)?;
    store.append_event(
        id,
        &r.cycle_id,
        "cancelled",
        serde_json::json!({ "by": "cli" }),
        true,
        now,
    )?;
    writeln!(
        out,
        "monitor {id} cancelled — the monitored work itself was NOT cancelled"
    )?;
    Ok(())
}

fn retry(
    store: &MonitorStore,
    id: &str,
    reset_budget: bool,
    out: &mut dyn Write,
    now: DateTime<Utc>,
) -> Result<()> {
    let r = resolve_id(store, id)?;
    let id = r.id.as_str();
    // In this slice, `hard_reached` is the ONLY way a monitor reaches
    // `Exhausted` — the remediation-attempt ceiling that would be the other
    // route is plan-2. `reactivate` deliberately does not (and must not)
    // clear `hard_reached`: a passed deadline is a fact about the work's
    // age, not a state to un-set, and clearing it would restart automatic
    // work past a deadline the spec says must stop it. So retrying a
    // hard-deadline exhaustion would be a guaranteed no-op: the next tick
    // observes once, sees `hard_reached` still true, and re-exhausts the
    // monitor — while this command would already have told the caller it
    // succeeded. Refuse before touching any state, rather than reactivate
    // and then lie about it; unlike the plan-2 remediation-ceiling case
    // (which genuinely can retry cleanly, below), there is nothing this
    // command can do to make the deadline case work, and this crate has no
    // verb to edit a spec's `hard_deadline` in place.
    if r.hard_reached {
        let deadline_at = r.work_started_at
            + chrono::Duration::from_std(r.spec.policy.hard_deadline())
                .unwrap_or(chrono::Duration::MAX);
        bail!(
            "monitor {id} cannot be usefully retried: its hard deadline ({}, {} after work \
             started at {}) has already passed. Reactivating would observe once and land back \
             in `exhausted` on the very next tick, reporting success for nothing. Register a \
             new monitor for this work, or the same spec with a longer `hard_deadline`, if it \
             is still worth watching — this slice has no verb to edit a deadline in place.",
            deadline_at.to_rfc3339(),
            r.spec.policy.hard_deadline,
            r.work_started_at.to_rfc3339(),
        );
    }
    if !store.reactivate(id, now, reset_budget)? {
        bail!(
            "only an exhausted monitor can be retried (state: {})",
            r.state.as_str()
        );
    }
    store.append_event(
        id,
        &r.cycle_id,
        "retried",
        serde_json::json!({ "reset_remediation_budget": reset_budget }),
        false,
        now,
    )?;
    writeln!(
        out,
        "monitor {id} reactivated{}",
        if reset_budget {
            ", remediation budget reset"
        } else {
            ""
        }
    )?;
    Ok(())
}

/// `RiskTier` has no `as_str`/`Display` — it round-trips through its own
/// `Serialize` (kebab-case), same as `mur_monitor::store::action::risk_to_sql`
/// does for the DB column, so a tier renamed in `mur_common::hitl` cannot
/// silently drift out of sync with what this prints.
fn risk_str(tier: RiskTier) -> String {
    match serde_json::to_value(tier) {
        Ok(serde_json::Value::String(s)) => s,
        _ => "?".to_string(),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).chain(std::iter::once('…')).collect()
    }
}

fn ago(now: DateTime<Utc>, t: DateTime<Utc>) -> String {
    human(now.signed_duration_since(t)) + " ago"
}

fn until(now: DateTime<Utc>, t: DateTime<Utc>) -> String {
    if t <= now {
        "due".into()
    } else {
        "in ".to_string() + &human(t.signed_duration_since(now))
    }
}

fn human(d: chrono::Duration) -> String {
    let s = d.num_seconds().max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h{}m", s / 3600, (s % 3600) / 60)
    }
}

#[cfg(test)]
#[path = "monitor_tests.rs"]
mod tests;
