//! `mur fleet review` — one attended, semi-auto review session (§3, §5, §7).
//!
//! Lifecycle: resolve limits (§9: a session whose limits cannot resolve does
//! not start) → create the ephemeral `review-<id>` fleet and its channel →
//! run [`run_review_loop`] with a terminal gate before every send (§5
//! semi-auto) → write `session_stopped` → remove the fleet definition but
//! keep the channel as the audit record (§7.1, A1) → print the stop screen
//! (§8.3).
//!
//! Auto mode needs a MURMUR session attached (§7), which this terminal entry
//! point is not, so it only runs semi-auto, and refuses to start without a
//! TTY rather than silently sending unattended.

use std::collections::BTreeSet;
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;
use std::time::Instant;

use anyhow::{Result, bail};
use mur_channel::ChannelService;
use mur_common::fleet::Fleet;
use mur_common::limits::Stuck;

use super::constants::{
    FLEET_CHANNEL_PREFIX, OPEN_HIGH_APPROVE_WARNING, REVIEW_FLEET_PREFIX,
    REVIEW_LEFT_PAUSED_NOTICE, REVIEW_PAUSED_CONTINUE_PROMPT, REVIEW_STOP_REASON_ESCALATION,
    RULING_NO_MAIN_REASON, RULING_POSITIONS, RULING_PROMPT, RULING_RECORDED_CONTINUE_PROMPT,
    RUNNING_LOCK, SEND_PROMPT, TRANSPORT_RETRY_DELAY,
};
use super::driver::{A2aTransport, ReviewTransport, SendAnswer};
use super::ledger::{EscalationRecord, Ledger};
use super::loop_driver::{LoopDriverStop, run_review_loop};
use super::note::{NoteLine, parse_note_line};
use super::resume::ResumeEnd;
use super::ruling::{is_rule_command, parse_rule_command};
use super::schema::{Cumulative, Mode, ReviewPayload, Role, SessionLimits, to_note_payload};
use super::wire::message_text;
use crate::cmd::fleet::loop_run::{LoopStop, fleet_bounds};
use crate::cmd::fleet::store;

/// Characters of the session id appended after [`REVIEW_FLEET_PREFIX`].
const SESSION_ID_LEN: usize = 8;

/// What the caller asked for (the clap args, minus parsing).
pub struct ReviewArgs {
    pub main: String,
    pub reviewer: String,
    pub task: String,
    pub deadline: Option<String>,
    pub budget_usd: Option<f64>,
}

/// §8.3: the stop reason as one word, also recorded in `session_stopped`.
pub fn stop_reason(stop: &LoopDriverStop) -> String {
    match stop {
        LoopDriverStop::Approve => "approve".into(),
        LoopDriverStop::Blocked {
            role: Role::Reviewer,
        } => "blocked (malformed verdict)".into(),
        LoopDriverStop::Blocked { role: Role::Main } => "blocked (malformed rebuttal)".into(),
        LoopDriverStop::ReviewerBlocked => "blocked".into(),
        LoopDriverStop::Stopped => "stopped".into(),
        // driver.rs already phrases this as "transport failure after one retry: …".
        LoopDriverStop::Paused { reason } => reason.clone(),
        LoopDriverStop::TaskFailed { member, cause } => {
            format!("{member} task failed: {cause}")
        }
        LoopDriverStop::RoundStuck => "stuck (round: open findings unchanged)".into(),
        LoopDriverStop::Escalation => REVIEW_STOP_REASON_ESCALATION.into(),
        LoopDriverStop::Guard(LoopStop::Deadline) => "limit: deadline".into(),
        LoopDriverStop::Guard(LoopStop::Stuck) => "limit: stuck (no activity)".into(),
        LoopDriverStop::Guard(LoopStop::Budget) => "limit: cost_usd".into(),
        LoopDriverStop::Guard(other) => format!("limit: {other:?}").to_lowercase(),
    }
}

/// A fresh, valid fleet name with the reserved review prefix (§7.1).
fn new_session_name() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("{REVIEW_FLEET_PREFIX}{}", &id[..SESSION_ID_LEN])
}

/// The session fleet's definition: two members, no limits of its own (they
/// come from the session flags and the global config via `fleet_bounds`).
/// The task is kept in `goal` so a paused session can be resumed (AC2).
fn session_fleet(name: &str, members: Vec<String>, channel_id: String, task: &str) -> Fleet {
    Fleet {
        name: name.to_string(),
        display_name: String::new(),
        goal: task.to_string(),
        router: None,
        team_id: None,
        members,
        channel_id,
        procedure: vec![],
        rules: vec![],
        skills: vec![],
        loop_cfg: None,
        parallel: None,
        hitl: None,
        requires_programs: vec![],
        limits: None,
        needs: vec![],
    }
}

/// Create the ephemeral two-member fleet and its channel. Bypasses
/// `cmd_fleet_create` because that command will refuse the reserved
/// `review-` prefix for user-created fleets (§7.1).
pub(super) fn create_session_fleet(
    mur_home: &Path,
    name: &str,
    main: &str,
    reviewer: &str,
    task: &str,
) -> Result<Fleet> {
    let members = vec![main.to_string(), reviewer.to_string()];
    let svc = ChannelService::open(mur_home)?;
    let ch = svc.create_for_fleet(name, crate::channel_writer::ROUTER_AGENT, &members)?;
    let fleet = session_fleet(name, members, ch.id, task);
    store::save_fleet(mur_home, &fleet)?;
    Ok(fleet)
}

/// §7.1 / A1: remove the fleet definition and run state; the channel stays.
pub(super) fn remove_session_fleet(mur_home: &Path, name: &str) -> Result<()> {
    for dir in [
        store::state_dir(mur_home, name),
        store::fleet_dir(mur_home, name),
    ] {
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
    }
    Ok(())
}

/// §4 `session_stopped`, signed by the same writer as every other event.
pub(super) fn append_session_stopped(
    mur_home: &Path,
    channel_id: &str,
    reason: &str,
    ledger: &Ledger,
) -> Result<()> {
    let payload = ReviewPayload::SessionStopped {
        reason: reason.to_string(),
        unresolved: ledger
            .stop_screen_findings(false)
            .into_iter()
            .map(|f| f.id.clone())
            .collect(),
        cumulative: Cumulative {
            exec_time_ms: ledger.exec_time_ms,
            cost_usd_micros: ledger.cost_usd_micros,
        },
    };
    let svc = ChannelService::open(mur_home)?;
    crate::channel_writer::append_as_writer(
        &svc,
        mur_home,
        channel_id,
        crate::channel_writer::ROUTER_AGENT,
        mur_common::channel::ChannelActor::System,
        mur_common::channel::EventKind::Note,
        to_note_payload(&payload),
        None,
    )?;
    Ok(())
}

/// §8.3 stop screen: reason plus every unresolved finding; after an approve,
/// open high findings are warned about first (#1721) and disputed
/// medium/low findings come first in the list.
pub fn render_stop_screen(stop: &LoopDriverStop, ledger: &Ledger, channel_id: &str) -> String {
    let mut out = format!("Review stopped: {}\n", stop_reason(stop));
    let after_approve = matches!(stop, LoopDriverStop::Approve);
    if after_approve {
        let open_high: Vec<&str> = ledger
            .open_high_severity()
            .iter()
            .map(|f| f.id.as_str())
            .collect();
        if !open_high.is_empty() {
            out.push_str(&format!(
                "{OPEN_HIGH_APPROVE_WARNING} {}\n",
                open_high.join(", ")
            ));
        }
    }
    let findings = ledger.stop_screen_findings(after_approve);
    if findings.is_empty() {
        out.push_str("No unresolved findings.\n");
    } else {
        out.push_str("Unresolved findings:\n");
        for f in findings {
            let severity = serde_json::to_value(f.severity).unwrap_or_default();
            let status = serde_json::to_value(f.status).unwrap_or_default();
            out.push_str(&format!(
                "  {} [{}, {}] {}\n",
                f.id,
                severity.as_str().unwrap_or_default(),
                status.as_str().unwrap_or_default(),
                f.issue
            ));
        }
    }
    if matches!(stop, LoopDriverStop::Paused { .. })
        && let Some(name) = channel_id.strip_prefix(FLEET_CHANNEL_PREFIX)
    {
        out.push_str(&format!(
            "Session paused. Resume with: mur fleet review-resume {name}\n"
        ));
    }
    out.push_str(&format!("Channel kept for audit: {channel_id}\n"));
    out
}

/// §3.5 human-input wait accumulated at this session's terminal, shared by
/// the send prompt and the tool-approval prompt; drained by the loop.
#[derive(Default)]
pub(super) struct HumanWait(std::cell::Cell<std::time::Duration>);

impl HumanWait {
    /// Run `ask` (a blocking terminal prompt) and count its time as wait.
    pub(super) fn time<R>(&self, ask: impl FnOnce() -> R) -> R {
        let t = Instant::now();
        let r = ask();
        self.0.set(self.0.get() + t.elapsed());
        r
    }
}

/// Reads one line from the human; `Ok("")` is EOF.
pub(super) type LineReader<'a> = &'a dyn Fn() -> std::io::Result<String>;
/// Writes prompt text to the human, without a trailing newline of its own.
pub(super) type TextWriter<'a> = &'a dyn Fn(&str) -> std::io::Result<()>;

/// Production [`LineReader`]: one line from stdin.
pub(super) fn stdin_line() -> std::io::Result<String> {
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line)
}

/// Production [`TextWriter`]: stdout, flushed so a prompt shows before a read.
pub(super) fn stdout_text(text: &str) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(text.as_bytes())?;
    out.flush()
}

/// §5 semi-auto over a terminal: show each outgoing message and send it
/// only when the human presses Enter; `q` declines (ends the session).
/// `input`/`output` are the terminal; tests inject their own.
pub(super) struct TerminalGate<'a, T> {
    pub(super) inner: T,
    pub(super) wait: &'a HumanWait,
    pub(super) input: LineReader<'a>,
    pub(super) output: TextWriter<'a>,
    /// `[main, reviewer]` — what `@<agent>` must resolve to (P3a-§3, N11).
    pub(super) members: [String; 2],
    /// Where `@<agent>` names are canonicalized.
    pub(super) mur_home: &'a Path,
}

impl<T: ReviewTransport> ReviewTransport for TerminalGate<'_, T> {
    fn send(&self, member: &str, params: &serde_json::Value) -> Result<String> {
        let reply = self.inner.send(member, params)?;
        (self.output)(&format!("\n--- reply from {member} ---\n{reply}\n\n"))?;
        Ok(reply)
    }

    fn confirm_send(
        &self,
        member: &str,
        params: &serde_json::Value,
        open: &BTreeSet<String>,
    ) -> Result<SendAnswer> {
        let text = message_text(params).unwrap_or_default();
        (self.output)(&format!("\n--- next message to {member} ---\n{text}\n"))?;
        loop {
            (self.output)(&format!("\n{}", SEND_PROMPT.replace("{member}", member)))?;
            let line = self.wait.time(|| (self.input)())?;
            if is_rule_command(line.trim()) {
                match parse_rule_command(&line, open) {
                    Ok(input) => return Ok(SendAnswer::SendWithRuling(input)),
                    Err(hint) => (self.output)(&format!("{hint}\n"))?,
                }
                continue;
            }
            let resolve = |n: &str| crate::a2a_dial::canonicalize_agent_name(self.mur_home, n);
            match parse_note_line(&line, &self.members, resolve) {
                NoteLine::Note(note) => return Ok(SendAnswer::Note(note)),
                NoteLine::Hint(hint) => (self.output)(&format!("{hint}\n"))?,
                NoteLine::NotNote => {
                    return Ok(if is_send_answer(&line) {
                        SendAnswer::Send
                    } else {
                        SendAnswer::Stop
                    });
                }
            }
        }
    }

    fn ask_ruling(&self, pending: &EscalationRecord, ledger: &Ledger) -> Result<String> {
        let id = pending.finding_id.as_str();
        let finding = ledger.finding(id);
        let positions = RULING_POSITIONS
            .replace("{id}", id)
            .replace("{reason}", &pending.reason)
            .replace(
                "{issue}",
                finding.map_or("", |f| {
                    f.last_reviewer_reason
                        .as_deref()
                        .unwrap_or(f.issue.as_str())
                }),
            )
            .replace(
                "{main}",
                finding
                    .and_then(|f| f.last_reject_reason.as_deref())
                    .unwrap_or(RULING_NO_MAIN_REASON),
            );
        (self.output)(&format!("{positions}{}", RULING_PROMPT.replace("{id}", id)))?;
        Ok(self.wait.time(|| (self.input)())?)
    }

    fn show(&self, text: &str) -> Result<()> {
        Ok((self.output)(&format!("{text}\n"))?)
    }

    fn take_human_wait(&self) -> std::time::Duration {
        self.wait.0.take() + self.inner.take_human_wait()
    }
}

/// A tool-approval request from inside a member's turn, asked at this
/// session's terminal (the session already refuses to start without a TTY).
/// Same tier rule as `murmur`'s plain mode: the prompt names the tier when
/// the call is above the auto ceiling, and only an explicit yes allows.
pub(super) fn ask_hitl(member: &str, hitl: &serde_json::Value) -> bool {
    use crate::cmd::agent::cli::stream::tool_tier_and_summary;
    let (tier, within_ceiling, summary) = tool_tier_and_summary(hitl);
    println!("\n--- {member} asks to run a tool ---");
    if !within_ceiling {
        println!("  [{tier:?} — above the auto ceiling]");
    }
    println!("  {summary}");
    print!("Allow? [y = allow, anything else = deny] ");
    if std::io::stdout().flush().is_err() {
        return false;
    }
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Enter (empty line) or `y`/`yes` sends; anything else, including EOF,
/// declines — an unreadable answer never sends.
fn is_send_answer(line: &str) -> bool {
    if line.is_empty() {
        return false; // EOF
    }
    matches!(line.trim().to_lowercase().as_str(), "" | "y" | "yes")
}

/// Refuse to start when a member is down, before main spends a turn only for
/// the reviewer's send to fail. Same liveness test as `a2a_dial`'s
/// `RequireRunning` (the lock file exists), so the two never disagree.
pub(super) fn require_running(mur_home: &Path, members: &[&str]) -> Result<()> {
    let down: Vec<&str> = members
        .iter()
        .copied()
        .filter(|m| !mur_home.join("agents").join(m).join(RUNNING_LOCK).exists())
        .collect();
    if down.is_empty() {
        return Ok(());
    }
    let names = down
        .iter()
        .map(|m| format!("'{m}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let starts = down
        .iter()
        .map(|m| format!("mur agent start {m}"))
        .collect::<Vec<_>>()
        .join(" && ");
    bail!("cannot start the review: {names} not running. Start with: {starts}");
}

/// `mur fleet review --main <a> --reviewer <b> "<task>"`.
pub fn cmd_fleet_review(mur_home: &Path, args: ReviewArgs) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!(
            "mur fleet review is attended: it asks before every send and needs a terminal. \
             Run it from an interactive shell."
        );
    }
    let canon = |n: &str| crate::a2a_dial::canonicalize_agent_name(mur_home, n);
    let (main, reviewer) = (canon(&args.main), canon(&args.reviewer));
    if main == reviewer {
        bail!("--main and --reviewer must be different agents (got '{main}' for both)");
    }
    if args.task.trim().is_empty() {
        bail!("the review task is empty: say what the main agent should do");
    }
    require_running(mur_home, &[&main, &reviewer])?;

    let name = new_session_name();
    // §9: resolve limits BEFORE anything is created, so an unresolvable
    // session leaves nothing behind. `fleet_bounds` needs only the fleet's
    // own (empty) limits block, so a stand-in with the final name is exact.
    let probe = session_fleet(&name, vec![], String::new(), "");
    let bounds = fleet_bounds(mur_home, &probe, args.deadline.as_deref(), args.budget_usd)?;
    let limits = SessionLimits::new(bounds.deadline, bounds.stuck, bounds.cost_usd);

    let fleet = create_session_fleet(mur_home, &name, &main, &reviewer, &args.task)?;
    println!(
        "Review session {name}: main = {main}, reviewer = {reviewer}, deadline {}, stuck {}.\n\
         Stop any time with `mur fleet stop {name}` or by answering q.",
        humantime_like(bounds.deadline),
        match bounds.stuck {
            Stuck::Off => "off".to_string(),
            Stuck::After(d) => humantime_like(d),
        },
    );

    let wait = HumanWait::default();
    let decide = |member: &str, hitl: &serde_json::Value| wait.time(|| ask_hitl(member, hitl));
    let transport = TerminalGate {
        inner: A2aTransport {
            mur_home,
            decide: &decide,
        },
        wait: &wait,
        input: &stdin_line,
        output: &stdout_text,
        members: [fleet.members[0].clone(), fleet.members[1].clone()],
        mur_home,
    };
    let (ledger, stop) = run_session(
        &transport,
        mur_home,
        &fleet,
        &args.task,
        limits,
        TRANSPORT_RETRY_DELAY,
    )?;
    print!(
        "\n{}",
        render_stop_screen(&stop, &ledger, &fleet.channel_id)
    );
    Ok(())
}

/// `mur fleet review-resume <session>` (AC2): rebuild a paused session from
/// its channel and continue at the same round, semi-auto, asking first.
pub fn cmd_fleet_review_resume(mur_home: &Path, name: &str) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!("mur fleet review-resume is attended and needs a terminal.");
    }
    let r = super::resume::prepare_resume(mur_home, name)?;
    require_running(mur_home, &[&r.fleet.members[0], &r.fleet.members[1]])?;
    if r.crashed {
        println!(
            "The previous driver stopped without pausing. Time counts up to its last recorded \
             event; a turn in flight then was not recorded and is re-run."
        );
    }
    println!(
        "{} at round {} with {} open finding(s); {} of {} used.",
        if r.crashed { "Crashed" } else { "Paused" },
        r.round,
        r.ledger.open_set().len(),
        humantime_like(r.active),
        humantime_like(r.limits.deadline()),
    );
    // P2-§6: branch on the ledger. A session that owes a ruling goes
    // straight to the ruling prompt (inside `settle_then_resume`); any
    // other asks to continue first.
    if r.ledger.pending_ruling().is_empty() {
        print!(
            "{}",
            if r.ruling_recorded {
                RULING_RECORDED_CONTINUE_PROMPT
            } else {
                REVIEW_PAUSED_CONTINUE_PROMPT
            }
        );
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        if !is_send_answer(&line) {
            println!("{REVIEW_LEFT_PAUSED_NOTICE}");
            return Ok(());
        }
    }
    let channel_id = r.fleet.channel_id.clone();
    let wait = HumanWait::default();
    let decide = |member: &str, hitl: &serde_json::Value| wait.time(|| ask_hitl(member, hitl));
    let transport = TerminalGate {
        inner: A2aTransport {
            mur_home,
            decide: &decide,
        },
        wait: &wait,
        input: &stdin_line,
        output: &stdout_text,
        members: [r.fleet.members[0].clone(), r.fleet.members[1].clone()],
        mur_home,
    };
    match super::resume::settle_then_resume(&transport, mur_home, r, TRANSPORT_RETRY_DELAY)? {
        ResumeEnd::LeftPaused => println!("{REVIEW_LEFT_PAUSED_NOTICE}"),
        ResumeEnd::Ran(ledger, stop) => {
            print!("\n{}", render_stop_screen(&stop, &ledger, &channel_id));
        }
    }
    Ok(())
}

/// Run the loop on an already-created session fleet, then end the session
/// whatever happened: record `session_stopped`, drop the fleet definition,
/// keep the channel (§7.1, A1). Split out so tests can inject a transport.
pub(super) fn run_session(
    transport: &dyn ReviewTransport,
    mur_home: &Path,
    fleet: &Fleet,
    task: &str,
    limits: SessionLimits,
    retry_delay: std::time::Duration,
) -> Result<(Ledger, LoopDriverStop)> {
    let [main, reviewer] = [&fleet.members[0], &fleet.members[1]];
    // §7.0: the run lock is held for the driver's whole life; the kernel
    // releases it however this process ends.
    let svc = ChannelService::open(mur_home)?;
    let _lock = super::run_lock::try_acquire(&svc, &fleet.channel_id)
        .map_err(|e| anyhow::anyhow!("review session '{}': {e}", fleet.name))?;
    let run = run_review_loop(
        transport,
        mur_home,
        &fleet.name,
        &fleet.channel_id,
        main,
        reviewer,
        task,
        Mode::SemiAuto,
        retry_delay,
        limits,
        &Instant::now,
    );
    end_session(mur_home, fleet, run)
}

/// End a session the loop returned from. A pause is NOT an end (§7, AC2):
/// the fleet definition and channel stay so `mur fleet review-resume` can
/// pick it up, and no `session_stopped` is written. Anything else records
/// `session_stopped`, drops the fleet definition, and keeps the channel.
pub(super) fn end_session(
    mur_home: &Path,
    fleet: &Fleet,
    run: Result<(Ledger, LoopDriverStop)>,
) -> Result<(Ledger, LoopDriverStop)> {
    let (ledger, stop) = match run {
        Ok(pair) => pair,
        Err(e) => {
            let _ = append_session_stopped(
                mur_home,
                &fleet.channel_id,
                &format!("error: {e}"),
                &Ledger::default(),
            );
            let _ = remove_session_fleet(mur_home, &fleet.name);
            return Err(e);
        }
    };
    if matches!(stop, LoopDriverStop::Paused { .. }) {
        return Ok((ledger, stop));
    }
    append_session_stopped(mur_home, &fleet.channel_id, &stop_reason(&stop), &ledger)?;
    remove_session_fleet(mur_home, &fleet.name)?;
    Ok((ledger, stop))
}

/// `1h 30m`-style rendering for the session banner.
fn humantime_like(d: std::time::Duration) -> String {
    let s = d.as_secs();
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    match (h, m, sec) {
        (0, 0, s) => format!("{s}s"),
        (0, m, 0) => format!("{m}m"),
        (h, 0, 0) => format!("{h}h"),
        (h, m, _) if h > 0 => format!("{h}h {m}m"),
        (_, m, s) => format!("{m}m {s}s"),
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod session_tests;
