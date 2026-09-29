use super::*;

/// Pre-dispatch triage settings (`triage:` in `~/.mur/config.yaml`).
///
/// Both knobs default to the passive posture, and `enforce` in particular is
/// off for a reason that is not caution: `triage_calibration` cannot score a
/// decision that prevented a run. Enforcing from the start produces only
/// unfalsifiable `held_back` records, so there would never be evidence that
/// enforcing was correct. Shadow mode records the same predictions AND lets
/// the run happen, which is what makes `Calibration::shadow_precision`
/// computable — turn `enforce` on once that number says triage is right often
/// enough to be worth the refusals.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TriageConfig {
    /// Run triage before dispatch at all. Off = not even the free prefilter
    /// runs, and no verdicts are recorded.
    #[serde(default)]
    pub enabled: bool,
    /// Let a triage decision actually stop a dispatch. Requires `enabled`.
    /// See the type docs before turning this on.
    #[serde(default)]
    pub enforce: bool,
}

impl TriageConfig {
    /// Does triage actually bind here? `enforce` alone is inert — with
    /// `enabled: false` nothing runs to enforce — so the two are resolved in
    /// one place rather than at each call site, where the pair would
    /// eventually be got wrong in one of them.
    pub fn enforces(&self) -> bool {
        self.enabled && self.enforce
    }
}

/// Rotation for `~/.mur/queue/events.jsonl`, in the shape FreeBSD's
/// `newsyslog(8)` uses: rotate past a size, keep a bounded number of
/// generations, compress all but the newest, drop the oldest.
///
/// The point of generations is that nobody has to decide to delete anything.
/// A 934 MB queue becomes `.0`, then `.1.gz`, and ages out on a policy the
/// user set rather than on a judgement call someone makes once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CaptureConfig {
    /// Rotate once the live file passes this. 64 MB keeps `mur hook stats`
    /// responsive — parsing is O(file), and 934 MB took minutes.
    #[serde(default = "default_rotate_at_mb")]
    pub rotate_at_mb: u64,
    /// How many rotated generations to keep. `.0` stays uncompressed like
    /// newsyslog's, the rest are gzipped.
    #[serde(default = "default_keep_generations")]
    pub keep_generations: u32,
}

pub(super) fn default_rotate_at_mb() -> u64 {
    64
}

fn default_keep_generations() -> u32 {
    5
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            rotate_at_mb: default_rotate_at_mb(),
            keep_generations: default_keep_generations(),
        }
    }
}

/// Post-upgrade settings for `mur update`. Stored under `update:` in
/// `~/.mur/config.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct UpdateConfig {
    /// macOS code-signing identity used to re-sign installed binaries after an
    /// upgrade. Keychain grants bind to the signing identity; fresh installs
    /// are ad-hoc (new CDHash per build), so without a stable identity every
    /// upgrade kills the grants and service-launched agents fail silently
    /// (#849/#866). Fallback: the `MUR_CODESIGN_IDENTITY` env var.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codesign_identity: Option<String>,
    /// Agent names `mur update --restart-agents` must never touch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restart_exclude: Vec<String>,
}

/// Which notification channels the durable monitor may use. `log` is not
/// listed: it is always on and cannot be disabled, because a notable event
/// must leave a trace somewhere even when a user has turned everything off.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NotificationsConfig {
    /// OS desktop notification. Opt-in: a background daemon that starts
    /// popping banners on upgrade is a hostile default.
    #[serde(default)]
    pub desktop: bool,
}

/// Whether the durable monitor may ask a model what to do about a terminal
/// failure the structured rules did not settle.
///
/// Off by default, and not merely as a courtesy: enabling it lets a
/// background daemon send monitor context to a model on its own schedule,
/// with no one watching. Nobody watching is not permission, so this is a
/// decision a user makes once, explicitly, rather than something an upgrade
/// makes for them.
///
/// The bound on how often it may ask is NOT here: one proposal per
/// observation cycle is an invariant of the design, not a knob (see
/// `mur_core::monitor::resolver`). A tunable would let a user turn a bounded
/// feature into an unbounded one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MonitorResolverConfig {
    /// Opt-in. While false, nothing in the resolver path runs and no request
    /// leaves the machine.
    #[serde(default)]
    pub enabled: bool,

    /// Which model to ask. `None` uses the same backend `mur chat` resolves,
    /// so a user who has already configured one does not configure it twice.
    #[serde(default)]
    pub model: Option<String>,
}

/// Run-status heartbeat tuning. Both values are config, never literals at a
/// call site: the right interval depends on how long the machine's steps take.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunsConfig {
    /// How often `execute_dag` stamps `last_heartbeat_at`.
    #[serde(default = "default_heartbeat_interval_secs")]
    pub heartbeat_interval_secs: u64,
    /// How many missed intervals before a live process counts as `stalled`.
    /// Three tolerates one lost tick plus scheduling jitter without calling a
    /// healthy run dead.
    #[serde(default = "default_heartbeat_stale_after_intervals")]
    pub heartbeat_stale_after_intervals: u32,
}

pub(super) fn default_heartbeat_interval_secs() -> u64 {
    10
}

pub(super) fn default_heartbeat_stale_after_intervals() -> u32 {
    3
}

impl Default for RunsConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval_secs: default_heartbeat_interval_secs(),
            heartbeat_stale_after_intervals: default_heartbeat_stale_after_intervals(),
        }
    }
}

/// Authorization gate for the `parallel_jobs` MCP tool. Stored under `parallel_jobs:`
/// in `~/.mur/config.yaml`. Deny-by-default: an empty `targets` list means the
/// tool cannot delegate to ANY agent (inert until the user opts specific
/// agents in). This is a deterministic, out-of-model gate that a
/// prompt-injected concierge cannot widen (OWASP Agentic ASI02/03/04).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ParallelJobsConfig {
    /// Canonical agent names the `parallel_jobs` tool is allowed to delegate to.
    /// Empty = deny all.
    #[serde(default)]
    pub targets: Vec<String>,
}

// --- memory-federation snapshot (spec 2026-08-04-unified-memory-federation) ---

/// Daemon-side settings for the signed snapshot pull. Stored under
/// `federation_snapshot:` in `~/.mur/config.yaml`; every field has a default
/// so an absent block means "defaults", never "off".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SnapshotConfig {
    /// How often the daemon sweeps `inbox/snapshot-requests/`, in seconds.
    pub poll_secs: u64,
    /// Reject requests older than this (replay blunting), in seconds.
    pub request_max_age_secs: u64,
    /// Minimum lifecycle state a global skill needs to enter a snapshot.
    pub min_lifecycle: crate::skill::stats::LifecycleState,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            poll_secs: 30,
            request_max_age_secs: 600,
            min_lifecycle: crate::skill::stats::LifecycleState::Stable,
        }
    }
}

/// Proactive memory capture (memory federation P2): gates the runtime's
/// built-in `remember` tool and its system-prompt directive. Stored under
/// `memory:` in `~/.mur/config.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MemoryConfig {
    pub capture: CaptureMode,
    /// How many memories may enter the system prompt per turn. Deliberately
    /// NOT `skills.max_skills_in_prompt`: sharing that budget means saving a
    /// memory silently evicts a skill, and five is a plausible number of
    /// standing preferences for one user to have.
    #[serde(default = "default_memory_in_prompt")]
    pub max_in_prompt: usize,
    /// Character ceiling for the whole memory block, spent before skills.
    #[serde(default = "default_memory_chars")]
    pub max_chars: usize,
}

fn default_memory_in_prompt() -> usize {
    20
}

fn default_memory_chars() -> usize {
    1500
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            capture: CaptureMode::AutoAnnounce,
            max_in_prompt: default_memory_in_prompt(),
            max_chars: default_memory_chars(),
        }
    }
}

/// How agents capture memories mid-conversation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// Agent asks for a one-line confirmation before saving.
    Ask,
    /// Agent saves immediately and announces in the same reply (default).
    AutoAnnounce,
    /// The `remember` tool is not registered at all.
    Off,
}

/// Authorization gate for the runtime's built-in `fleet_run` tool. Stored under
/// `fleet_run:` in `~/.mur/config.yaml`. Deny-by-default on BOTH axes: an agent
/// not named in `agents` never even sees the tool, and a fleet not named in
/// `fleets` cannot be run. Lives in the global config (not the agent profile)
/// because the profile is writable by the concierge itself — this gate must be
/// out of reach of a prompt-injected agent (same rationale as
/// [`ParallelJobsConfig`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FleetRunConfig {
    /// Canonical agent names allowed to call `fleet_run`. Empty = deny all.
    #[serde(default)]
    pub agents: Vec<String>,
    /// Fleet names those agents may run. Empty = deny all.
    #[serde(default)]
    pub fleets: Vec<String>,
}

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

/// Daemon-wide gate for unattended fleet auto-run (`mur-daemon`'s `fleet_tick`).
/// Stored under `fleet:` in `~/.mur/config.yaml`. Either this flag OR the
/// `MUR_FLEET_AUTORUN` env var satisfies the gate — both are equally explicit,
/// off-by-default opt-ins; the env var remains for ops/CI use, this flag is
/// what the Hub's Settings toggle controls. Per-fleet `budget_usd > 0` and the
/// `.stopped` kill-switch are unaffected — see `mur-daemon/src/fleet_tick.rs`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FleetConfig {
    /// Allow fleets with a trigger + budget configured to auto-run unattended.
    #[serde(default)]
    pub autorun: bool,
}

/// Routing for Anthropic subscription-OAuth (`sk-ant-oat*`) tokens through a
/// local bridge — cc-proxy — that swaps `x-api-key` for the Bearer +
/// claude-code betas disguise the upstream requires.
///
/// The Hub injects `ANTHROPIC_BASE_URL` pointing at [`url`](Self::url) when it
/// spawns an agent runtime, but only when [`enabled`](Self::enabled) is set and
/// the bridge is actually listening; otherwise it leaves the runtime on the
/// direct `api.anthropic.com` path (where an oat token would 401). A runtime
/// launched with `ANTHROPIC_BASE_URL` already in its environment is never
/// overridden.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CcProxyConfig {
    /// Bridge base URL. Defaults to cc-proxy's default bind.
    #[serde(default = "default_cc_proxy_url")]
    pub url: String,

    /// Master switch. When false the Hub never routes runtimes through the
    /// bridge, regardless of reachability.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_cc_proxy_url() -> String {
    "http://127.0.0.1:8088".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for CcProxyConfig {
    fn default() -> Self {
        Self {
            url: default_cc_proxy_url(),
            enabled: true,
        }
    }
}

/// Configuration for the agent CLI TUI.
/// Stored in ~/.mur/config.yaml under the `cli:` key.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CliConfig {
    /// Default visual skin for `mur agent cli`. Overridable with --skin.
    /// Valid values: "ansi" (default; "dark" is an alias), "light", "mur".
    pub skin: Option<String>,

    /// True once the one-time "permanent instructions" notice has been shown
    /// (memories P1 §10).
    ///
    /// The notice is informational and must appear at most once — repeating a
    /// feature announcement every session reads as a warning about something
    /// wrong. Absent (false) in existing configs, which is correct: a user who
    /// has never seen it should.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub seen_permanent_instructions_notice: bool,
}

/// Configuration for the mobile relay (P4).
/// Stored in ~/.mur/config.yaml under the `mobile_relay:` key.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MobileRelayConfig {
    /// Base URL of the mur-server relay, e.g. "wss://relay.mur.run".
    /// Leave blank to disable relay forwarding on the Mac daemon side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,

    /// API key or JWT used by the Mac daemon to authenticate with the relay.
    /// The value is typically a `mur_...` API key from app.mur.run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[cfg(test)]
mod fleet_config_tests {
    use super::*;

    #[test]
    fn fleet_config_defaults_off_and_roundtrips() {
        assert!(!FleetConfig::default().autorun);

        let cfg: Config = serde_yaml_ng::from_str("fleet:\n  autorun: true\n").unwrap();
        assert!(cfg.fleet.autorun);

        // `fleet:` key entirely absent → defaults to off
        let cfg2: Config = serde_yaml_ng::from_str("{}").unwrap();
        assert!(!cfg2.fleet.autorun);
    }

    #[test]
    fn fleet_run_config_defaults_deny_all_and_roundtrips() {
        // Absent section → both allowlists empty → deny all.
        let cfg: Config = serde_yaml_ng::from_str("{}").unwrap();
        assert!(cfg.fleet_run.agents.is_empty());
        assert!(cfg.fleet_run.fleets.is_empty());

        let cfg2: Config =
            serde_yaml_ng::from_str("fleet_run:\n  agents: [mur]\n  fleets: [deep-research]\n")
                .unwrap();
        assert_eq!(cfg2.fleet_run.agents, vec!["mur"]);
        assert_eq!(cfg2.fleet_run.fleets, vec!["deep-research"]);
    }
}

#[cfg(test)]
mod runs_config_tests {
    use super::*;

    /// A zero `heartbeat_interval_secs` is legal YAML but illegal at runtime:
    /// it zeroes the stale threshold (every live run instantly reads STALLED)
    /// and `tokio::time::interval(Duration::ZERO)` panics in the executor's
    /// ticker. The loader must clamp zeroes to the defaults so one
    /// user-edited line can neither lie nor crash.
    #[test]
    fn zero_heartbeat_values_clamp_to_defaults_at_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.yaml");
        std::fs::write(
            &path,
            "runs:\n  heartbeat_interval_secs: 0\n  heartbeat_stale_after_intervals: 0\n",
        )
        .unwrap();

        let cfg = Config::load_or_default(&path);
        assert_eq!(
            cfg.runs.heartbeat_interval_secs, 10,
            "a zero interval must load as the default, not 0"
        );
        assert_eq!(
            cfg.runs.heartbeat_stale_after_intervals, 3,
            "a zero interval count must load as the default, not 0"
        );
    }

    /// The clamp must not rewrite legitimate tuning: positive values survive.
    #[test]
    fn positive_heartbeat_values_survive_the_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.yaml");
        std::fs::write(
            &path,
            "runs:\n  heartbeat_interval_secs: 60\n  heartbeat_stale_after_intervals: 2\n",
        )
        .unwrap();

        let cfg = Config::load_or_default(&path);
        assert_eq!(cfg.runs.heartbeat_interval_secs, 60);
        assert_eq!(cfg.runs.heartbeat_stale_after_intervals, 2);
    }
}

#[cfg(test)]
mod ambient_capture_cfg_tests {
    use super::*;

    #[test]
    fn session_and_harvest_defaults() {
        let cfg: Config = serde_yaml::from_str("{}").unwrap();
        assert_eq!(cfg.session.capture, "ambient");
        assert_eq!(cfg.session.retention_days, 14);
        assert!(cfg.harvest.auto_gate);
        assert_eq!(cfg.harvest.llm, "local-first");
        assert_eq!(cfg.harvest.min_events, 5);
        assert_eq!(cfg.harvest.min_user_turns, 2);
        assert_eq!(cfg.harvest.min_duration_secs, 120);
        assert_eq!(cfg.harvest.idle_minutes, 30);
        assert_eq!(cfg.harvest.max_llm_calls_per_day, 10);
        assert_eq!(cfg.harvest.max_extract_input_tokens, 12000);
        assert!(cfg.harvest.session_start_hint);
        assert!((cfg.harvest.similarity_merge_threshold - 0.6).abs() < f32::EPSILON);
    }

    #[test]
    fn session_capture_override_parses() {
        let cfg: Config =
            serde_yaml::from_str("session:\n  capture: off\n  retention_days: 3\n").unwrap();
        assert_eq!(cfg.session.capture, "off");
        assert_eq!(cfg.session.retention_days, 3);
    }
}

#[cfg(test)]
mod cc_proxy_cfg_tests {
    use super::*;

    #[test]
    fn defaults_to_local_cc_proxy_enabled() {
        let cfg: Config = serde_yaml_ng::from_str("{}").unwrap();
        assert_eq!(cfg.cc_proxy.url, "http://127.0.0.1:8088");
        assert!(cfg.cc_proxy.enabled);
    }

    #[test]
    fn url_and_enabled_override_parse() {
        let cfg: Config =
            serde_yaml_ng::from_str("cc_proxy:\n  url: http://127.0.0.1:9999\n  enabled: false\n")
                .unwrap();
        assert_eq!(cfg.cc_proxy.url, "http://127.0.0.1:9999");
        assert!(!cfg.cc_proxy.enabled);
    }

    #[test]
    fn partial_section_keeps_other_default() {
        // Only `enabled` given → url stays at the default.
        let cfg: Config = serde_yaml_ng::from_str("cc_proxy:\n  enabled: false\n").unwrap();
        assert_eq!(cfg.cc_proxy.url, "http://127.0.0.1:8088");
        assert!(!cfg.cc_proxy.enabled);
    }
}

#[cfg(test)]
mod notifications_config_tests {
    use super::*;

    #[test]
    fn notifications_default_to_log_only() {
        let c: Config = serde_yaml::from_str("{}").unwrap();
        assert!(!c.notifications.desktop, "desktop must be opt-in");
    }

    #[test]
    fn an_existing_config_without_the_block_still_parses() {
        // Every user upgrading has a config.yaml with no `notifications:` key.
        let c: Config = serde_yaml::from_str("retrieval:\n  min_score: 0.42\n").unwrap();
        assert!(!c.notifications.desktop);
    }

    /// The whole safety argument for the resolver rests on this one bit: an
    /// upgrade must never start letting the daemon talk to a model. Asserted
    /// from both an empty config and a realistic existing one, because the
    /// failure that matters is an upgrade, not a fresh install.
    #[test]
    fn the_monitor_resolver_is_off_until_a_user_turns_it_on() {
        let empty: Config = serde_yaml::from_str("{}").unwrap();
        assert!(
            !empty.monitor_resolver.enabled,
            "asking a model must be opt-in"
        );
        assert!(empty.monitor_resolver.model.is_none());

        let upgraded: Config = serde_yaml::from_str(
            "retrieval:\n  min_score: 0.42\nnotifications:\n  desktop: true\n",
        )
        .unwrap();
        assert!(
            !upgraded.monitor_resolver.enabled,
            "a config written before this feature existed must not enable it"
        );
    }

    #[test]
    fn the_monitor_resolver_reads_back_what_a_user_wrote() {
        let c: Config =
            serde_yaml::from_str("monitor_resolver:\n  enabled: true\n  model: claude_haiku\n")
                .unwrap();
        assert!(c.monitor_resolver.enabled);
        assert_eq!(c.monitor_resolver.model.as_deref(), Some("claude_haiku"));
    }

    /// Triage must be inert for everyone who never asked for it — including
    /// every config file written before it existed.
    #[test]
    fn triage_is_off_and_non_binding_by_default() {
        let c: Config = serde_yaml::from_str("retrieval:\n  min_score: 0.42\n").unwrap();
        assert!(!c.triage.enabled, "triage must not run unasked");
        assert!(!c.triage.enforce);
        assert!(!c.triage.enforces());
    }

    /// The trap this guards: `enforce: true` alone looks like it turned
    /// something on, but there is nothing running to enforce.
    #[test]
    fn enforce_without_enabled_binds_nothing() {
        let c: Config = serde_yaml::from_str("triage:\n  enforce: true\n").unwrap();
        assert!(c.triage.enforce, "the user's word is kept verbatim");
        assert!(
            !c.triage.enforces(),
            "but it binds nothing while triage does not run"
        );
    }

    #[test]
    fn triage_binds_only_when_both_knobs_are_set() {
        let c: Config =
            serde_yaml::from_str("triage:\n  enabled: true\n  enforce: true\n").unwrap();
        assert!(c.triage.enforces());
    }

    /// Enabled without enforce is shadow mode: it runs, it records, it never
    /// stops anything.
    #[test]
    fn enabled_alone_is_shadow_mode() {
        let c: Config = serde_yaml::from_str("triage:\n  enabled: true\n").unwrap();
        assert!(c.triage.enabled);
        assert!(!c.triage.enforces());
    }
}
