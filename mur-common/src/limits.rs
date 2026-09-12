//! Execution limits: the one `limits:` block every scope carries (spec
//! 2026-09-12 execution-limits §3.1), its duration grammar, the built-in
//! defaults (§3.3) and the resolver that says what is in force and where it
//! came from (§3.9).
//!
//! Three knobs, no more: `deadline` (wall clock for the unit of work),
//! `stuck` (minutes of no progress before a stop, or `off`) and `cost_usd`
//! (only meaningful on a metered model — applicability is the caller's call,
//! this module resolves the number). Inner scopes REPLACE a key; nothing here
//! adds two caps together, because the product of caps is the problem the
//! spec exists to remove.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The block as written in YAML. Every key optional; an absent key means
/// "inherit", never "unlimited" — `stuck: off` is the explicit opt-out.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl Limits {
    pub fn is_empty(&self) -> bool {
        self.deadline.is_none() && self.stuck.is_none() && self.cost_usd.is_none()
    }
}

/// The stuck detector's setting once resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stuck {
    Off,
    After(Duration),
}

/// Where a resolved value came from — the half of `mur limits` that makes it
/// worth running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    BuiltIn,
    Global,
    Fleet,
    Agent,
    Flag,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::BuiltIn => "built-in default",
            Source::Global => "~/.mur/config.yaml",
            Source::Fleet => "fleet.yaml",
            Source::Agent => "profile.yaml",
            Source::Flag => "command-line flag",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resolved<T> {
    pub value: T,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedLimits {
    pub deadline: Resolved<Option<Duration>>,
    pub stuck: Resolved<Stuck>,
    pub cost_usd: Resolved<Option<f64>>,
}

/// Which built-in deadline applies when no scope sets one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    FleetRun,
    SingleTask,
}

/// §3.3 — the constants `mur limits` prints as `(built-in default)`.
pub const DEFAULT_STUCK: Duration = Duration::from_secs(10 * 60);
pub const DEFAULT_DEADLINE_FLEET: Duration = Duration::from_secs(60 * 60);
pub const DEFAULT_DEADLINE_TASK: Duration = Duration::from_secs(30 * 60);

/// `30s`, `5m`, `2h`, `1d`, `1h30m`, or a bare integer (seconds). `None` on
/// anything else — the caller turns that into a load error naming the key.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<u64>() {
        return Some(Duration::from_secs(n));
    }
    let mut total: u64 = 0;
    let mut num = String::new();
    let mut saw_unit = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: u64 = num.parse().ok()?;
        num.clear();
        let mult = match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return None,
        };
        total = total.checked_add(n.checked_mul(mult)?)?;
        saw_unit = true;
    }
    if !num.is_empty() || !saw_unit {
        return None;
    }
    Some(Duration::from_secs(total))
}

fn parse_stuck(s: &str) -> Option<Stuck> {
    if s.trim().eq_ignore_ascii_case("off") {
        return Some(Stuck::Off);
    }
    parse_duration(s).map(Stuck::After)
}

/// §4: an unparsable value is an error naming the key, never a silent default.
pub fn validate(l: &Limits) -> Result<(), String> {
    if let Some(d) = &l.deadline
        && parse_duration(d).is_none()
    {
        return Err(format!(
            "limits.deadline: `{d}` is not a duration (30s, 5m, 2h, 1h30m, 1d)"
        ));
    }
    if let Some(s) = &l.stuck
        && parse_stuck(s).is_none()
    {
        return Err(format!("limits.stuck: `{s}` is not a duration or `off`"));
    }
    if let Some(c) = l.cost_usd
        && !(c.is_finite() && c >= 0.0)
    {
        return Err(format!(
            "limits.cost_usd: `{c}` must be a non-negative number"
        ));
    }
    Ok(())
}

/// The resolver. Precedence, narrowest wins: flag > agent > fleet > global >
/// built-in. Each key is resolved on its own; a scope that sets only `stuck`
/// leaves `deadline` to the next scope out.
pub fn resolve(
    scope: Scope,
    global: &Limits,
    fleet: Option<&Limits>,
    agent: Option<&Limits>,
    flags: &Limits,
) -> Result<ResolvedLimits, String> {
    for (l, who) in [
        (Some(flags), "flag"),
        (agent, "profile.yaml"),
        (fleet, "fleet.yaml"),
        (Some(global), "config.yaml"),
    ] {
        if let Some(l) = l {
            validate(l).map_err(|e| format!("{who}: {e}"))?;
        }
    }
    // Narrowest first; the first scope that carries the key wins.
    let layers: [(Option<&Limits>, Source); 4] = [
        (Some(flags), Source::Flag),
        (agent, Source::Agent),
        (fleet, Source::Fleet),
        (Some(global), Source::Global),
    ];
    let pick = |get: &dyn Fn(&Limits) -> bool| -> Option<(&Limits, Source)> {
        layers
            .iter()
            .find_map(|(l, src)| l.filter(|l| get(l)).map(|l| (l, *src)))
    };

    let deadline = match pick(&|l| l.deadline.is_some()) {
        Some((l, src)) => Resolved {
            value: l.deadline.as_deref().and_then(parse_duration),
            source: src,
        },
        None => Resolved {
            value: Some(match scope {
                Scope::FleetRun => DEFAULT_DEADLINE_FLEET,
                Scope::SingleTask => DEFAULT_DEADLINE_TASK,
            }),
            source: Source::BuiltIn,
        },
    };
    let stuck = match pick(&|l| l.stuck.is_some()) {
        Some((l, src)) => Resolved {
            value: l
                .stuck
                .as_deref()
                .and_then(parse_stuck)
                .unwrap_or(Stuck::After(DEFAULT_STUCK)),
            source: src,
        },
        None => Resolved {
            value: Stuck::After(DEFAULT_STUCK),
            source: Source::BuiltIn,
        },
    };
    let cost_usd = match pick(&|l| l.cost_usd.is_some()) {
        Some((l, src)) => Resolved {
            value: l.cost_usd,
            source: src,
        },
        None => Resolved {
            value: None,
            source: Source::BuiltIn,
        },
    };
    Ok(ResolvedLimits {
        deadline,
        stuck,
        cost_usd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(deadline: Option<&str>, stuck: Option<&str>, cost: Option<f64>) -> Limits {
        Limits {
            deadline: deadline.map(str::to_string),
            stuck: stuck.map(str::to_string),
            cost_usd: cost,
        }
    }

    #[test]
    fn durations_parse_the_spec_grammar_and_nothing_else() {
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("1d"), Some(Duration::from_secs(86_400)));
        assert_eq!(parse_duration("1h30m"), Some(Duration::from_secs(5400)));
        assert_eq!(parse_duration(" 90 "), Some(Duration::from_secs(90)));
        for bad in ["", "off", "2 hours", "1h3", "h", "-5m", "1.5h"] {
            assert_eq!(parse_duration(bad), None, "{bad:?} must not parse");
        }
    }

    #[test]
    fn narrowest_scope_wins_per_key_and_never_combines() {
        let global = l(Some("4h"), Some("20m"), Some(50.0));
        let fleet = l(Some("2h"), None, None);
        let agent = l(None, Some("off"), None);
        let r = resolve(
            Scope::FleetRun,
            &global,
            Some(&fleet),
            Some(&agent),
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.deadline.value, Some(Duration::from_secs(7200)));
        assert_eq!(
            r.deadline.source,
            Source::Fleet,
            "fleet set it, agent did not"
        );
        assert_eq!(r.stuck.value, Stuck::Off);
        assert_eq!(r.stuck.source, Source::Agent);
        assert_eq!(r.cost_usd.value, Some(50.0));
        assert_eq!(r.cost_usd.source, Source::Global, "nobody narrower set it");

        // A flag beats everyone, for its key only.
        let flags = l(Some("10m"), None, None);
        let r = resolve(Scope::FleetRun, &global, Some(&fleet), Some(&agent), &flags).unwrap();
        assert_eq!(r.deadline.source, Source::Flag);
        assert_eq!(r.stuck.source, Source::Agent);
    }

    #[test]
    fn built_in_defaults_fill_what_no_scope_set_and_say_so() {
        let none = Limits::default();
        let r = resolve(Scope::FleetRun, &none, None, None, &none).unwrap();
        assert_eq!(r.deadline.value, Some(DEFAULT_DEADLINE_FLEET));
        assert_eq!(r.deadline.source, Source::BuiltIn);
        assert_eq!(r.stuck.value, Stuck::After(DEFAULT_STUCK));
        assert_eq!(r.cost_usd.value, None);
        let r = resolve(Scope::SingleTask, &none, None, None, &none).unwrap();
        assert_eq!(r.deadline.value, Some(DEFAULT_DEADLINE_TASK));
    }

    #[test]
    fn a_bad_value_is_an_error_that_names_the_key_and_the_scope() {
        let bad = l(Some("soon"), None, None);
        let e = resolve(
            Scope::FleetRun,
            &Limits::default(),
            Some(&bad),
            None,
            &Limits::default(),
        )
        .unwrap_err();
        assert!(
            e.contains("fleet.yaml") && e.contains("limits.deadline") && e.contains("soon"),
            "{e}"
        );
        let e = validate(&l(None, Some("sometimes"), None)).unwrap_err();
        assert!(e.contains("limits.stuck"), "{e}");
        let e = validate(&l(None, None, Some(-1.0))).unwrap_err();
        assert!(e.contains("limits.cost_usd"), "{e}");
    }

    #[test]
    fn unknown_keys_are_rejected_at_load() {
        let e = serde_yaml_ng::from_str::<Limits>("deadline: 1h\nmax_iterations: 5\n").unwrap_err();
        assert!(e.to_string().contains("max_iterations"), "{e}");
        let ok: Limits = serde_yaml_ng::from_str("stuck: off\n").unwrap();
        assert_eq!(ok.stuck.as_deref(), Some("off"));
        assert!(serde_yaml_ng::to_string(&Limits::default()).unwrap().trim() == "{}");
    }
}
