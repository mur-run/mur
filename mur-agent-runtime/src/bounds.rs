//! What bounds one turn (spec 2026-09-12 execution-limits §3.2–§3.5).
//!
//! Resolution is `mur_common::limits::resolve` over the scopes the runtime can
//! see — `config.yaml` and the agent's own profile — plus the caller's
//! remaining clock as the innermost layer, so a task delegated by a fleet with
//! twelve minutes left gets twelve minutes, not a fresh half hour (§3.4). The
//! attended split is decided here and nowhere else: a turn somebody is
//! watching has no deadline and a stuck clock that only warns.

use std::time::{Duration, Instant};

use mur_common::limits::{Limits, Scope, Source, Stuck, resolve};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnBounds {
    pub attended: bool,
    /// `None` when attended, or when a scope set an unparsable value that
    /// validation already rejected upstream (never reached in practice).
    pub deadline: Option<Instant>,
    pub stuck: Stuck,
    pub deadline_source: Source,
}

/// `caller_deadline_secs` is the innermost scope: the remaining time the
/// launching fleet has. It rides the `Flag` layer of the resolver because that
/// is exactly what it is — a per-call value that beats every file.
pub fn resolve_bounds(
    attended: bool,
    global: &Limits,
    agent: Option<&Limits>,
    caller_deadline_secs: Option<u64>,
    now: Instant,
) -> Result<TurnBounds, String> {
    let flags = Limits {
        deadline: caller_deadline_secs.map(|s| format!("{s}s")),
        stuck: None,
        cost_usd: None,
    };
    let r = resolve(Scope::SingleTask, global, None, agent, &flags)?;
    Ok(TurnBounds {
        attended,
        deadline: if attended {
            None
        } else {
            r.deadline.value.map(|d| now + d)
        },
        stuck: r.stuck.value,
        deadline_source: r.deadline.source,
    })
}

/// The stuck clock. `note_progress` resets it; `stuck_for` is how long the
/// turn has gone without a progress signal.
#[derive(Debug, Clone)]
pub struct Progress {
    last: Instant,
    /// The previous iteration's `(tool, args-fingerprint)` set — a call that
    /// repeats one of these is not progress (§3.5).
    prev_calls: Vec<(String, u64)>,
    /// Last three calls, newest last, for the stop reason.
    recent: std::collections::VecDeque<String>,
}

impl Progress {
    pub fn start(now: Instant) -> Self {
        Self {
            last: now,
            prev_calls: Vec::new(),
            recent: std::collections::VecDeque::with_capacity(3),
        }
    }

    /// Feed one iteration's tool calls. Progress iff at least one call wrote a
    /// file or differs from every call of the previous iteration. A text-only
    /// iteration (`calls` empty) is never progress.
    pub fn observe(&mut self, calls: &[(String, u64)], now: Instant) {
        let progressed = calls.iter().any(|(tool, fp)| {
            is_file_write(tool) || !self.prev_calls.iter().any(|(t, f)| t == tool && f == fp)
        });
        for (tool, _) in calls {
            if self.recent.len() == 3 {
                self.recent.pop_front();
            }
            self.recent.push_back(tool.clone());
        }
        if progressed {
            self.last = now;
        }
        self.prev_calls = calls.to_vec();
    }

    pub fn stuck_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last)
    }

    /// `write_file, bash, bash` — what the stop reason shows.
    pub fn last_calls(&self) -> String {
        if self.recent.is_empty() {
            "no tool calls".to_string()
        } else {
            self.recent.iter().cloned().collect::<Vec<_>>().join(", ")
        }
    }
}

/// `10m`, `2h`, `45s` — the same spelling `mur limits` prints.
pub fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s == 0 {
        return "0s".to_string();
    }
    if s.is_multiple_of(3600) {
        format!("{}h", s / 3600)
    } else if s.is_multiple_of(60) {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

/// The tools that write files, by name. Kept as a list here rather than a
/// trait method because the fleet loop's channel-event rule (§3.5) is the
/// other half of "progress" and lives in mur-core; both sides stay data.
fn is_file_write(tool: &str) -> bool {
    matches!(tool, "write_file" | "edit_file" | "append_file")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(deadline: Option<&str>, stuck: Option<&str>) -> Limits {
        Limits {
            deadline: deadline.map(str::to_string),
            stuck: stuck.map(str::to_string),
            cost_usd: None,
        }
    }

    /// §3.2: attended has no deadline whatever the files say; unattended gets
    /// the narrowest scope, and the caller's remaining clock beats the files.
    #[test]
    fn attended_has_no_deadline_and_the_callers_clock_wins_unattended() {
        let now = Instant::now();
        let global = l(Some("4h"), Some("20m"));
        let agent = l(Some("45m"), None);
        let a = resolve_bounds(true, &global, Some(&agent), Some(720), now).unwrap();
        assert_eq!(a.deadline, None);
        assert_eq!(
            a.stuck,
            Stuck::After(Duration::from_secs(20 * 60)),
            "stuck still resolves — it warns"
        );
        let u = resolve_bounds(false, &global, Some(&agent), Some(720), now).unwrap();
        assert_eq!(
            u.deadline,
            Some(now + Duration::from_secs(720)),
            "the fleet's remaining twelve minutes"
        );
        assert_eq!(u.deadline_source, Source::Flag);
        let u = resolve_bounds(false, &global, Some(&agent), None, now).unwrap();
        assert_eq!(u.deadline, Some(now + Duration::from_secs(45 * 60)));
        assert_eq!(u.deadline_source, Source::Agent);
        let u = resolve_bounds(false, &Limits::default(), None, None, now).unwrap();
        assert_eq!(
            u.deadline,
            Some(now + mur_common::limits::DEFAULT_DEADLINE_TASK)
        );
        assert_eq!(u.deadline_source, Source::BuiltIn);
    }

    /// §3.5: identical calls are not progress; a file write always is; a
    /// text-only iteration never is.
    #[test]
    fn progress_resets_only_on_new_calls_or_file_writes() {
        let t0 = Instant::now();
        let mut p = Progress::start(t0);
        let bash = ("bash".to_string(), 1u64);
        p.observe(std::slice::from_ref(&bash), t0 + Duration::from_secs(10));
        assert_eq!(
            p.stuck_for(t0 + Duration::from_secs(10)),
            Duration::ZERO,
            "first call is new"
        );
        p.observe(std::slice::from_ref(&bash), t0 + Duration::from_secs(20));
        assert_eq!(
            p.stuck_for(t0 + Duration::from_secs(20)),
            Duration::from_secs(10),
            "same call again: clock keeps running"
        );
        p.observe(&[], t0 + Duration::from_secs(30));
        assert_eq!(
            p.stuck_for(t0 + Duration::from_secs(30)),
            Duration::from_secs(20),
            "text-only turn: not progress"
        );
        p.observe(
            &[("write_file".to_string(), 1)],
            t0 + Duration::from_secs(40),
        );
        assert_eq!(
            p.stuck_for(t0 + Duration::from_secs(40)),
            Duration::ZERO,
            "a file write is always progress"
        );
        p.observe(
            &[("write_file".to_string(), 1)],
            t0 + Duration::from_secs(50),
        );
        assert_eq!(
            p.stuck_for(t0 + Duration::from_secs(50)),
            Duration::ZERO,
            "even the same write again"
        );
        assert_eq!(p.last_calls(), "bash, write_file, write_file");
    }
}
