//! Dispatch index: derived facts about the agents and fleets on this machine.
//!
//! Every field here is READ from what the kernel actually enforces
//! (`entitlements`) plus the profile's own metadata. Nothing is declared in a
//! separate list, so the index cannot drift from reality the way a hand-written
//! capability table would — a new agent appears the moment its `profile.yaml`
//! exists, with no registration step.
//!
//! Three properties this module deliberately keeps apart:
//!
//! * **Hard** (`exec`, `writes`, `net`) — kernel-enforced, authoritative in the
//!   NEGATIVE direction: absent from the allowlist means the agent physically
//!   cannot do it. Present does NOT mean it is good at it. Use to FILTER.
//! * **Soft** (`role`, `skills`, `model_ref`) — human-written or heuristic, can
//!   be stale or overstated. Use to RANK and to explain, never to filter.
//! * **Authorization** (`FleetFacts::authorized`) — read from the global config
//!   the agents cannot write. Never inferred, never widened here.
//!
//! Lives in `mur-common` because both consumers need it and the dependency
//! graph is `mur-core -> mur-agent-runtime -> mur-common`: the runtime's bash
//! tool cannot reach mur-core, so anything shared has to sit at the bottom.

use std::path::{Path, PathBuf};

use crate::agent::{AgentProfile, NetworkOutboundMode, SpawnMode};
use crate::fleet::Fleet;

/// What the sandbox lets an agent exec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecFacts {
    /// `spawn.mode: any` — no exec restriction at all.
    Unrestricted,
    /// Binaries named in `spawn.allowed`.
    Allowlist(Vec<String>),
    /// `spawn.mode: none` / `strict` with nothing granted.
    Nothing,
}

/// One agent, as the kernel and the profile describe it.
#[derive(Debug, Clone)]
pub struct AgentFacts {
    pub name: String,
    /// `profile.role`, falling back to a non-boilerplate `persona.description`.
    /// Empty means nobody has said what this agent is for.
    pub role: String,
    pub exec: ExecFacts,
    pub writes: Vec<PathBuf>,
    pub net: NetworkOutboundMode,
    pub skills: Vec<String>,
    pub model_ref: String,
    /// Per-turn effort from the profile. `None` means unset — which is the API
    /// default (`high`), not "no effort"; `mur agent who` says so explicitly
    /// because the difference is the whole point.
    pub effort: Option<crate::llm::Effort>,
    pub running: bool,
    /// `profile.yaml` (or `sys_prompt.md`) was edited after the running process
    /// started, so the live agent is NOT what this index describes. See
    /// [`started_after_edits`].
    pub drift: bool,
}

impl AgentFacts {
    /// Does this agent explicitly hold `bin`?
    ///
    /// `bin` may be a bare name (`cargo`) or the absolute path the kernel
    /// refused (`/Users/d/.cargo/bin/cargo`) — a denial always reports the
    /// latter, so both sides are compared by file name. An allowlist entry may
    /// itself be absolute, which is why the normalisation is symmetric.
    ///
    /// Deliberately CONSERVATIVE: under `Allowlist` mode the sandbox also
    /// re-allows the system exec paths (`/usr/bin`, `/bin`, …), so an agent can
    /// in fact run `/usr/bin/git` without naming it. Resolving that would mean
    /// replicating the runtime's `PATH` augmentation and Seatbelt's system-path
    /// exemption down here, and it would answer the wrong question anyway: a
    /// binary that resolves to a system path is one nobody needed to delegate.
    /// Under-reporting routes work to an agent that provably holds the binary;
    /// over-reporting would route it to one that dies with the same EPERM.
    pub fn can_exec(&self, bin: &str) -> bool {
        fn base(s: &str) -> &str {
            Path::new(s)
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or(s)
        }
        match &self.exec {
            ExecFacts::Unrestricted => true,
            ExecFacts::Nothing => false,
            ExecFacts::Allowlist(list) => {
                let want = base(bin);
                list.iter().any(|b| b == bin || base(b) == want)
            }
        }
    }

    /// Can it write anywhere at or under `path`?
    pub fn can_write(&self, path: &Path) -> bool {
        self.writes.iter().any(|w| path.starts_with(w))
    }

    /// How much privilege this agent carries, for least-privilege dispatch
    /// (P4): among the agents that CAN do the job, prefer the one carrying the
    /// least unrelated power. Without this the ranking silently prefers the
    /// most capable agent — which is the one that undoes every containment
    /// decision made elsewhere.
    ///
    /// A heuristic, and openly so: writable roots dominate (they are what an
    /// escaped task can damage), then egress (what it can exfiltrate to), then
    /// breadth of exec.
    pub fn privilege_breadth(&self) -> u32 {
        let writes = self.writes.len() as u32 * 4;
        let net = match self.net {
            NetworkOutboundMode::Unrestricted => 8,
            NetworkOutboundMode::Restricted => 2,
            NetworkOutboundMode::ProxyOnly => 1,
            NetworkOutboundMode::Off => 0,
        };
        let exec = match &self.exec {
            // Anything at all: strictly broader than any enumerable list.
            ExecFacts::Unrestricted => 100,
            ExecFacts::Allowlist(l) => l.len() as u32,
            ExecFacts::Nothing => 0,
        };
        writes + net + exec
    }
}

/// Why a fleet cannot be dispatched to right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocker {
    /// Not in `fleet_run.fleets` (or the caller is not in `fleet_run.agents`).
    NotAuthorized,
    /// `fleet_run` refuses fleets without an enforced budget.
    NoBudget,
}

impl Blocker {
    pub fn as_str(&self) -> &'static str {
        match self {
            Blocker::NotAuthorized => "not authorized for this agent",
            Blocker::NoBudget => "no loop.budget_usd",
        }
    }
}

/// One fleet plus the members that give it its abilities. A fleet declares no
/// capabilities of its own — it has exactly what its members have.
#[derive(Debug, Clone)]
pub struct FleetFacts {
    pub name: String,
    pub members: Vec<AgentFacts>,
    pub budget_usd: f64,
    pub authorized: bool,
}

impl FleetFacts {
    pub fn blocker(&self) -> Option<Blocker> {
        if !self.authorized {
            Some(Blocker::NotAuthorized)
        } else if self.budget_usd <= 0.0 {
            Some(Blocker::NoBudget)
        } else {
            None
        }
    }

    /// The members that could actually run `bin`.
    pub fn members_with(&self, bin: &str) -> Vec<&AgentFacts> {
        self.members.iter().filter(|m| m.can_exec(bin)).collect()
    }

    /// Least-privileged capable member's breadth — the fleet is only as broad
    /// as the member that will end up doing the work.
    fn breadth_for(&self, bin: &str) -> u32 {
        self.members_with(bin)
            .iter()
            .map(|m| m.privilege_breadth())
            .min()
            .unwrap_or(u32::MAX)
    }

    fn covers_cwd(&self, bin: &str, cwd: Option<&Path>) -> bool {
        match cwd {
            None => true,
            Some(c) => self.members_with(bin).iter().any(|m| m.can_write(c)),
        }
    }
}

/// Routes for one denied binary, split by whether they can be used right now.
///
/// The split is the security design, not presentation: `ready` is safe to put
/// in an agent's context, `blocked` is for the HUMAN. A list of "powerful
/// fleets you are not allowed to use" handed to a prompt-injected agent is an
/// attack map; handed to the user it is the grant path that stops them from
/// disabling the sandbox out of frustration.
#[derive(Debug, Clone, Default)]
pub struct ExecRoutes {
    pub ready: Vec<FleetFacts>,
    pub blocked: Vec<FleetFacts>,
}

/// Expand `~` and `{{agent_home}}` the way the runtime's profile loader does.
fn expand(raw: &str, home: &Path, agent_home: &Path) -> PathBuf {
    let s = raw.replace("{{agent_home}}", &agent_home.to_string_lossy());
    if let Some(rest) = s.strip_prefix("~/") {
        home.join(rest)
    } else if s == "~" {
        home.to_path_buf()
    } else {
        PathBuf::from(s)
    }
}

/// Skill names, from BOTH places a profile records them: the `installed_skills`
/// cards and the bare `skills:` path refs (`skills/idiomatic-rust-2024`) that
/// most agents actually use. Reading only the former left the soft layer empty
/// for nearly every agent on a real machine, which would make a capability
/// roster worthless.
///
/// Takes plain lists rather than the profile so it stays testable without a
/// sixty-field `AgentProfile` fixture.
fn merge_skill_names(installed: Vec<String>, refs: &[String]) -> Vec<String> {
    let mut out = installed;
    for r in refs {
        let name = r.rsplit('/').next().unwrap_or(r).trim();
        if !name.is_empty() && !out.iter().any(|s| s == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// True when the running process started AFTER the newest edit to the files it
/// loads once at boot. `perm allow-spawn` warns exactly once at edit time and
/// nothing reminds you afterwards, so an index built from disk alone will
/// happily claim an ability the live process does not have.
///
/// Uses `running.lock`'s `started_at` against file mtimes — the profile digest
/// in that lock is computed by the runtime after `{{agent_home}}` expansion, so
/// recomputing it here would mean duplicating loader logic across a crate
/// boundary for a strictly worse reason.
fn started_after_edits(agent_dir: &Path) -> Option<bool> {
    let lock = std::fs::read_to_string(agent_dir.join("running.lock")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&lock).ok()?;
    let started = chrono::DateTime::parse_from_rfc3339(v.get("started_at")?.as_str()?).ok()?;
    let newest = ["profile.yaml", "sys_prompt.md"]
        .iter()
        .filter_map(|f| std::fs::metadata(agent_dir.join(f)).ok()?.modified().ok())
        .max()?;
    let newest: chrono::DateTime<chrono::Utc> = newest.into();
    Some(started.with_timezone(&chrono::Utc) >= newest)
}

/// Read one agent's facts. `None` when there is no readable profile — every
/// filesystem failure here is "this agent contributes nothing to the index",
/// never a hard error: a dispatch hint must not fail because one agent
/// directory is broken.
pub fn agent_facts(mur_home: &Path, name: &str) -> Option<AgentFacts> {
    let agent_dir = mur_home.join("agents").join(name);
    let raw = std::fs::read_to_string(agent_dir.join("profile.yaml")).ok()?;
    let p: AgentProfile = serde_yaml_ng::from_str(&raw).ok()?;
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));

    let exec = match p.entitlements.processes.spawn.mode {
        SpawnMode::Any => ExecFacts::Unrestricted,
        SpawnMode::None => ExecFacts::Nothing,
        SpawnMode::Allowlist | SpawnMode::Strict => {
            let allowed = p.entitlements.processes.spawn.allowed.clone();
            if allowed.is_empty() {
                ExecFacts::Nothing
            } else {
                ExecFacts::Allowlist(allowed)
            }
        }
    };

    // `persona.description` is auto-filled with "Agent <name>" at creation
    // (cmd/agent/lifecycle.rs), which says nothing — treat it as unset rather
    // than showing it as a role.
    let boilerplate = format!("Agent {name}");
    let role = p
        .role
        .clone()
        .filter(|r| !r.trim().is_empty())
        .or_else(|| {
            Some(p.persona.description.clone()).filter(|d| !d.is_empty() && *d != boilerplate)
        })
        .unwrap_or_default();

    let running = agent_dir.join("running.lock").is_file();
    Some(AgentFacts {
        name: name.to_string(),
        role,
        exec,
        writes: p
            .entitlements
            .filesystem
            .write
            .iter()
            .map(|w| expand(w, &home, &agent_dir))
            .collect(),
        net: p.entitlements.network.outbound.mode,
        skills: merge_skill_names(
            p.installed_skills.iter().map(|s| s.name.clone()).collect(),
            &p.skills,
        ),
        model_ref: p.model_ref.clone().unwrap_or_default(),
        effort: p.effort,
        // Not running => nothing to drift from; the next start reads disk.
        drift: running && started_after_edits(&agent_dir) == Some(false),
        running,
    })
}

/// Every agent with a readable profile, sorted by name.
pub fn scan_agents(mur_home: &Path) -> Vec<AgentFacts> {
    let mut out: Vec<AgentFacts> = std::fs::read_dir(mur_home.join("agents"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (!name.starts_with('.')).then(|| agent_facts(mur_home, &name))?
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Every fleet, with each member's facts resolved and authorization decided
/// for `requester` (the agent that would call `fleet_run`).
pub fn scan_fleets(mur_home: &Path, requester: &str) -> Vec<FleetFacts> {
    let cfg = crate::config::Config::load_or_default(&mur_home.join("config.yaml")).fleet_run;
    let caller_ok = cfg.agents.iter().any(|a| a == requester);

    let mut out: Vec<FleetFacts> = std::fs::read_dir(mur_home.join("fleets"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let raw = std::fs::read_to_string(e.path().join("fleet.yaml")).ok()?;
            let f: Fleet = serde_yaml_ng::from_str(&raw).ok()?;
            let name = f.name.clone();
            Some(FleetFacts {
                members: f
                    .members
                    .iter()
                    .filter_map(|m| agent_facts(mur_home, m))
                    .collect(),
                budget_usd: f.loop_cfg.map(|l| l.budget_usd).unwrap_or(0.0),
                authorized: caller_ok && cfg.fleets.contains(&name),
                name,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Which fleets can run `bin` (optionally, in a directory they can write).
///
/// Ranking, best first: fleets whose capable member can also write `cwd`, then
/// LEAST privilege (P4), then name for determinism.
pub fn who_can_exec(mur_home: &Path, requester: &str, bin: &str, cwd: Option<&Path>) -> ExecRoutes {
    let mut routes = ExecRoutes::default();
    for f in scan_fleets(mur_home, requester) {
        if f.members_with(bin).is_empty() {
            continue;
        }
        if f.blocker().is_some() {
            routes.blocked.push(f);
        } else {
            routes.ready.push(f);
        }
    }
    let rank = |f: &FleetFacts| (!f.covers_cwd(bin, cwd), f.breadth_for(bin), f.name.clone());
    routes.ready.sort_by_key(rank);
    routes.blocked.sort_by_key(rank);
    routes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(name: &str, exec: ExecFacts, writes: &[&str], net: NetworkOutboundMode) -> AgentFacts {
        AgentFacts {
            name: name.into(),
            role: String::new(),
            exec,
            writes: writes.iter().map(PathBuf::from).collect(),
            net,
            skills: vec![],
            model_ref: String::new(),
            effort: None,
            running: true,
            drift: false,
        }
    }

    #[test]
    fn can_exec_is_conservative_and_path_aware() {
        let a = facts(
            "a",
            ExecFacts::Allowlist(vec!["cargo".into(), "/opt/x/bin/tool".into()]),
            &[],
            NetworkOutboundMode::Off,
        );
        assert!(a.can_exec("cargo"));
        // The kernel reports the ABSOLUTE path it refused, which is what the
        // dispatch hint hands us — matching only bare names here left every
        // route empty in the live path.
        assert!(a.can_exec("/Users/d/.cargo/bin/cargo"));
        // An absolute allowlist entry still matches a bare request.
        assert!(a.can_exec("tool"));
        assert!(a.can_exec("/opt/x/bin/tool"));
        // Reachable via /usr/bin in reality, but not explicitly held: we do NOT
        // claim it (see can_exec's doc).
        assert!(!a.can_exec("git"));
        assert!(!a.can_exec("/usr/bin/git"));
        // Basename matching must not turn a different binary into a match.
        assert!(!a.can_exec("/evil/cargo-nope"));

        assert!(facts("b", ExecFacts::Unrestricted, &[], NetworkOutboundMode::Off).can_exec("git"));
        assert!(!facts("c", ExecFacts::Nothing, &[], NetworkOutboundMode::Off).can_exec("git"));
    }

    #[test]
    fn privilege_breadth_prefers_the_narrow_agent() {
        let narrow = facts(
            "narrow",
            ExecFacts::Allowlist(vec!["cargo".into()]),
            &["/repo"],
            NetworkOutboundMode::Restricted,
        );
        let wide = facts(
            "wide",
            ExecFacts::Allowlist(vec!["cargo".into(), "git".into(), "curl".into()]),
            &["/repo", "/other", "/home"],
            NetworkOutboundMode::Unrestricted,
        );
        assert!(narrow.privilege_breadth() < wide.privilege_breadth());
        // Unrestricted exec must outrank any enumerable allowlist.
        let any = facts(
            "any",
            ExecFacts::Unrestricted,
            &[],
            NetworkOutboundMode::Off,
        );
        assert!(any.privilege_breadth() > wide.privilege_breadth());
    }

    #[test]
    fn can_write_matches_subpaths_only() {
        let a = facts(
            "a",
            ExecFacts::Nothing,
            &["/repo/mur"],
            NetworkOutboundMode::Off,
        );
        assert!(a.can_write(Path::new("/repo/mur")));
        assert!(a.can_write(Path::new("/repo/mur/src/lib.rs")));
        assert!(!a.can_write(Path::new("/repo/other")));
        // Prefix-of-a-sibling must not match (`/repo/mur2` vs `/repo/mur`).
        assert!(!a.can_write(Path::new("/repo/mur2")));
    }

    #[test]
    fn skill_names_merge_refs_dedup_and_drop_empties() {
        let out = merge_skill_names(
            vec!["code-review".into()],
            &[
                "skills/rust-async".into(),
                // Already present as an installed card — must not double up.
                "skills/code-review".into(),
                "".into(),
                "bare-name".into(),
            ],
        );
        assert_eq!(out, vec!["code-review", "rust-async", "bare-name"]);
    }

    #[test]
    fn blocker_reports_authorization_before_budget() {
        let mut f = FleetFacts {
            name: "f".into(),
            members: vec![],
            budget_usd: 0.0,
            authorized: false,
        };
        assert_eq!(f.blocker(), Some(Blocker::NotAuthorized));
        f.authorized = true;
        assert_eq!(f.blocker(), Some(Blocker::NoBudget));
        f.budget_usd = 1.0;
        assert_eq!(f.blocker(), None);
    }

    #[test]
    fn fleet_breadth_is_the_narrowest_capable_member() {
        let f = FleetFacts {
            name: "f".into(),
            members: vec![
                facts(
                    "wide",
                    ExecFacts::Allowlist(vec!["cargo".into()]),
                    &["/a", "/b", "/c"],
                    NetworkOutboundMode::Unrestricted,
                ),
                facts(
                    "narrow",
                    ExecFacts::Allowlist(vec!["cargo".into()]),
                    &["/a"],
                    NetworkOutboundMode::Restricted,
                ),
                // Cannot run it at all — must not lower the fleet's breadth.
                facts("idle", ExecFacts::Nothing, &[], NetworkOutboundMode::Off),
            ],
            budget_usd: 1.0,
            authorized: true,
        };
        assert_eq!(f.members_with("cargo").len(), 2);
        assert_eq!(f.breadth_for("cargo"), 4 + 2 + 1);
    }
}
