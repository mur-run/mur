//! Fleet — a named squad of agents working a shared goal over one channel.

use serde::{Deserialize, Serialize};

use crate::deps::ProgramDep;
use crate::parallel::ParallelConfig;

pub const CONCIERGE_AGENT: &str = "mur";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fleet {
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router: Option<String>,
    /// Team identifier for this fleet; set when the fleet is affiliated with a
    /// MUR Server team. The fleet runner sets MUR_ACTIVE_TEAM from this value
    /// before each member turn so team-scoped skills inject correctly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(default)]
    pub members: Vec<String>,
    pub channel_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, rename = "loop", skip_serializing_if = "Option::is_none")]
    pub loop_cfg: Option<FleetLoop>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel: Option<ParallelConfig>,
    /// How this fleet handles risk-tiered approvals. Absent → auto: defer when
    /// nothing can answer (no TTY), wait when a terminal is attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hitl: Option<FleetHitl>,
    /// External programs this artifact needs at runtime (portable-deps spec).
    /// Absent → empty; resolved by `mur agent/fleet doctor` + `install-deps`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_programs: Vec<ProgramDep>,
}

/// Per-fleet approval policy. A floor, never a grant: every field here can only
/// make an outcome stricter or change WHO waits — none of them approves
/// anything, and there is deliberately no "auto-approve" knob (that is what
/// `--yes` is, and it stays unreachable from unattended fleet paths).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetHitl {
    /// What an unanswered Ask-tier gate does. Absent → auto: defer when no TTY
    /// is attached, wait when one is. Set it explicitly when the TTY is a bad
    /// proxy for "somebody is watching" — a monitored ops fleet wants `wait`
    /// even headless; a fleet that must never reach for a human wants `deny`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<crate::hitl::Unanswered>,
    /// Risk tiers this fleet's owner has taken standing responsibility for:
    /// the gate approves them without asking, and records the auto-approval on
    /// the channel so the run is still auditable.
    ///
    /// An explicit LIST, not a ceiling — `RiskTier`'s ordering would make
    /// `up_to: spend` quietly cover `network-egress` too, and a grant nobody
    /// meant to write is the whole failure mode this feature has to avoid.
    /// [`crate::hitl::tier_may_be_granted`] bounds what may appear here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auto_approve_tiers: Vec<crate::hitl::RiskTier>,
}

impl FleetHitl {
    /// Reject a policy that grants more than config is allowed to grant.
    ///
    /// Loud at load time rather than silently ignored at the gate: a user who
    /// wrote `destructive` here believes it took effect, and the gap between
    /// that belief and the truth is where the damage lives.
    pub fn validate(&self) -> Result<(), String> {
        let bad: Vec<String> = self
            .auto_approve_tiers
            .iter()
            .filter(|t| !crate::hitl::tier_may_be_granted(**t))
            .map(|t| format!("{t:?}").to_lowercase())
            .collect();
        if bad.is_empty() {
            return Ok(());
        }
        Err(format!(
            "hitl.auto_approve_tiers may not include {} — standing approval is capped at `write`. \
             Those actions have to be approved per-run (`mur channel approve`), because their cost \
             cannot be undone by noticing afterwards.",
            bad.join(", ")
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetLoop {
    #[serde(default = "default_trigger")]
    pub trigger: String,
    // default 0 → resolvers fall back (cap → DEFAULT_MAX_ITERATIONS, budget → no cap),
    // so a minimal `loop:` block (e.g. just `trigger:`) deserializes.
    #[serde(default)]
    pub max_iterations: u32,
    #[serde(default)]
    pub budget_usd: f64,
    #[serde(default)]
    pub deadline: String,
    #[serde(default)]
    pub done_when: String,
}

fn default_trigger() -> String {
    "manual".to_string()
}

impl Fleet {
    pub fn router_or_concierge(&self) -> &str {
        self.router.as_deref().unwrap_or(CONCIERGE_AGENT)
    }
}

/// A fleet name must be a filesystem-safe lowercase slug (it becomes a directory
/// `~/.mur/fleets/<name>` and a channel id `fleet-<name>`).
pub fn valid_fleet_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Channel-id prefix for a fleet's shared channel (`fleet-<name>`).
pub const CHANNEL_PREFIX: &str = "fleet-";

/// Derive the fleet name from a channel id of the form `fleet-<name>`.
///
/// Returns `None` for non-fleet channels, and also for a `fleet-`-prefixed id
/// whose remainder isn't a valid fleet name — so a crafted channel id can't
/// smuggle a path-traversal segment or otherwise masquerade as a fleet to pull
/// in fleet-scoped skills.
pub fn fleet_name_from_channel_id(channel_id: &str) -> Option<&str> {
    let name = channel_id.strip_prefix(CHANNEL_PREFIX)?;
    valid_fleet_name(name).then_some(name)
}

/// Job status lifecycle: queued → running → {done, failed, canceled}, with
/// `blocked` as a non-terminal detour: the run reached an action that needs a
/// human and stopped there. An approval resumes it, so it is deliberately NOT
/// terminal — reporting it as done would claim work nobody did, and reporting
/// it as failed would claim a fault that did not occur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Blocked,
    Done,
    Failed,
    Canceled,
}

impl JobStatus {
    /// Returns true if the job has reached a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobStatus::Done | JobStatus::Failed | JobStatus::Canceled
        )
    }

    /// Lowercase name — matches the serde representation and the A2A `TaskState`
    /// mapping. Use this for display instead of `{:?}`/`Debug`, which is not a
    /// stable display contract.
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Blocked => "blocked",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
            JobStatus::Canceled => "canceled",
        }
    }
}

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A job represents a unit of work submitted to a fleet for execution.
/// The `id` is a UUIDv7 — time-sortable, so FIFO ordering is just a filename sort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub text: String,
    /// "cli" | "a2a:<agent-id>" (a2a follow-on).
    pub source: String,
    pub status: JobStatus,
    /// RFC3339 timestamps.
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Channel run executed job (results live there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_fleet_name_accepts_and_rejects() {
        // accepted
        assert!(valid_fleet_name("dev"));
        assert!(valid_fleet_name("dev-team"));
        assert!(valid_fleet_name("dev_1"));
        assert!(valid_fleet_name("ab12"));
        // rejected
        assert!(!valid_fleet_name("")); // empty
        assert!(!valid_fleet_name("../x")); // path traversal
        assert!(!valid_fleet_name("a/b")); // slash
        assert!(!valid_fleet_name("a\\b")); // backslash
        assert!(!valid_fleet_name("Dev")); // uppercase
        assert!(!valid_fleet_name("a b")); // space
        assert!(!valid_fleet_name(".hidden")); // dot
    }

    #[test]
    fn fleet_name_from_channel_id_extracts_and_validates() {
        // valid fleet channels
        assert_eq!(fleet_name_from_channel_id("fleet-dev"), Some("dev"));
        assert_eq!(
            fleet_name_from_channel_id("fleet-my-squad"),
            Some("my-squad")
        );
        assert_eq!(fleet_name_from_channel_id("fleet-ab12"), Some("ab12"));
        // not a fleet channel
        assert_eq!(fleet_name_from_channel_id("dev"), None);
        assert_eq!(fleet_name_from_channel_id("agent:foo:uuid"), None);
        // prefixed but invalid remainder → rejected (no masquerading)
        assert_eq!(fleet_name_from_channel_id("fleet-"), None); // empty
        assert_eq!(fleet_name_from_channel_id("fleet-../etc"), None); // traversal
        assert_eq!(fleet_name_from_channel_id("fleet-a/b"), None); // slash
        assert_eq!(fleet_name_from_channel_id("fleet-Dev"), None); // uppercase
    }

    #[test]
    fn fleet_minimal_yaml_deserializes_with_defaults() {
        let f: Fleet = serde_yaml::from_str("name: dev\nchannel_id: fleet-dev\n").unwrap();
        assert_eq!(f.name, "dev");
        assert_eq!(f.channel_id, "fleet-dev");
        assert!(f.members.is_empty());
        assert_eq!(f.router_or_concierge(), CONCIERGE_AGENT);
        assert!(f.loop_cfg.is_none());
    }

    /// `hitl.mode` must survive a round-trip and stay absent when unset — an
    /// absent policy means "use the TTY-derived default", which is a different
    /// statement from any of the three explicit modes.
    #[test]
    fn fleet_hitl_mode_parses_and_defaults_to_absent() {
        let bare: Fleet = serde_yaml::from_str("name: dev\nchannel_id: fleet-dev\n").unwrap();
        assert!(bare.hitl.is_none(), "no policy stated ⇒ auto");

        let declared: Fleet =
            serde_yaml::from_str("name: dev\nchannel_id: fleet-dev\nhitl:\n  mode: deny\n")
                .unwrap();
        assert_eq!(
            declared.hitl.as_ref().and_then(|h| h.mode),
            Some(crate::hitl::Unanswered::Deny)
        );

        for (yaml, want) in [
            ("defer", crate::hitl::Unanswered::Defer),
            ("wait", crate::hitl::Unanswered::Wait),
            ("deny", crate::hitl::Unanswered::Deny),
        ] {
            let f: Fleet = serde_yaml::from_str(&format!(
                "name: dev\nchannel_id: fleet-dev\nhitl:\n  mode: {yaml}\n"
            ))
            .unwrap();
            assert_eq!(f.hitl.and_then(|h| h.mode), Some(want));
        }

        // An unknown mode is a typo, not a silent fallback to something looser.
        assert!(
            serde_yaml::from_str::<Fleet>(
                "name: dev\nchannel_id: fleet-dev\nhitl:\n  mode: yolo\n"
            )
            .is_err()
        );
    }

    /// `auto_approve_tiers` parses; `validate` accepts write-and-below and
    /// refuses everything above — loudly, because a user who wrote
    /// `destructive` believes it took effect, and the gap between that belief
    /// and the truth is where the damage lives.
    #[test]
    fn auto_approve_tiers_parse_and_validate_against_the_ceiling() {
        let f: Fleet = serde_yaml::from_str(
            "name: dev\nchannel_id: fleet-dev\nhitl:\n  auto_approve_tiers: [read, write]\n",
        )
        .unwrap();
        let hitl = f.hitl.as_ref().unwrap();
        assert_eq!(
            hitl.auto_approve_tiers,
            vec![crate::hitl::RiskTier::Read, crate::hitl::RiskTier::Write]
        );
        assert!(hitl.validate().is_ok());

        let f: Fleet = serde_yaml::from_str(
            "name: dev\nchannel_id: fleet-dev\nhitl:\n  auto_approve_tiers: [write, destructive]\n",
        )
        .unwrap();
        let err = f.hitl.as_ref().unwrap().validate().unwrap_err();
        assert!(err.contains("destructive"), "{err}");
        assert!(err.contains("write"), "the ceiling must be named: {err}");
    }

    #[test]
    fn fleet_yaml_roundtrip_and_router_default() {
        let f = Fleet {
            name: "dev".into(),
            display_name: "Dev Team".into(),
            goal: "ship it".into(),
            router: None,
            team_id: None,
            members: vec!["pm".into(), "qa".into()],
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
            parallel: None,
            hitl: None,
            requires_programs: vec![],
        };
        assert_eq!(f.router_or_concierge(), CONCIERGE_AGENT);
        let yaml = serde_yaml::to_string(&f).unwrap();
        let back: Fleet = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back, f);
        // `loop:` key (not `loop_cfg`) when present
        let with_loop: Fleet = serde_yaml::from_str(
            "name: dev\ndisplay_name: Dev\ngoal: test\nchannel_id: fleet-dev\nrules: []\nskills: []\nmembers: []\nloop:\n  trigger: manual\n  max_iterations: 3\n  budget_usd: 1.0\n  deadline: '2026-12-31'\n  done_when: 'all_tasks_done'\n",
        ).unwrap();
        assert_eq!(with_loop.loop_cfg.unwrap().max_iterations, 3);
    }

    #[test]
    fn minimal_loop_block_deserializes_with_defaults() {
        // A `loop:` block with only a trigger must not fail (max_iterations /
        // budget_usd default to 0 → resolvers fall back).
        let f: Fleet = serde_yaml::from_str(
            "name: dev\nchannel_id: fleet-dev\nloop:\n  trigger: \"interval:1h\"\n",
        )
        .unwrap();
        let l = f.loop_cfg.unwrap();
        assert_eq!(l.trigger, "interval:1h");
        assert_eq!(l.max_iterations, 0);
        assert_eq!(l.budget_usd, 0.0);
    }

    #[test]
    fn job_status_serde_is_lowercase_and_terminal_predicate() {
        assert_eq!(
            serde_yaml::to_string(&JobStatus::Queued).unwrap().trim(),
            "queued"
        );
        assert_eq!(
            serde_yaml::to_string(&JobStatus::Done).unwrap().trim(),
            "done"
        );
        assert!(!JobStatus::Queued.is_terminal());
        assert!(!JobStatus::Running.is_terminal());
        assert!(JobStatus::Done.is_terminal());
        assert!(JobStatus::Failed.is_terminal());
        assert!(JobStatus::Canceled.is_terminal());
    }

    #[test]
    fn job_status_as_str_and_display_match_serde_for_all_variants() {
        for s in [
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Done,
            JobStatus::Failed,
            JobStatus::Canceled,
        ] {
            let serde = serde_yaml::to_string(&s).unwrap();
            assert_eq!(
                serde.trim(),
                s.as_str(),
                "as_str must match serde for {s:?}"
            );
            assert_eq!(s.to_string(), s.as_str(), "Display must delegate to as_str");
        }
    }

    #[test]
    fn job_yaml_roundtrip_with_optional_fields_skipped() {
        let j = Job {
            id: "0190f3a2-0000-7000-8000-000000000000".into(),
            text: "ship it".into(),
            source: "cli".into(),
            status: JobStatus::Queued,
            created_at: "2026-06-24T00:00:00Z".into(),
            started_at: None,
            finished_at: None,
            run_id: None,
            result: None,
            error: None,
        };
        let yaml = serde_yaml::to_string(&j).unwrap();
        assert!(
            !yaml.contains("started_at"),
            "None optionals must be skipped: {yaml}"
        );
        let back: Job = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back, j);
    }
}
