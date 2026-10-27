//! MUR's own launch chain: the files that decide what starts next and with
//! what authority.
//!
//! A sandbox that can be edited from inside it is not a boundary, it is a
//! delay. The set guarded here is deliberately NOT "dangerous paths" — that
//! set is open-ended (`.zshenv`, autostart, git hooks, cron) and unwinnable.
//! It is the closed set MUR owns: what triggers a start, what gets exec'd,
//! what entitlements the started process carries, and what identity it signs
//! with. Every member is derivable from `mur_home`, `bin_dir` and `$HOME`.
//!
//! This is a predicate, not a path list, on purpose: a list built at seal
//! time cannot cover `<mur_home>/agents/<name>` for a name that did not exist
//! yet, and creating that directory is exactly the escape.

use std::path::{Path, PathBuf};

/// Written by MUR itself under `bin_dir`; exec'd before any sandbox applies.
const RUNTIME_BINARY: &str = "mur-agent-runtime";
/// BusyBox-style per-agent symlinks to `RUNTIME_BINARY`.
const AGENT_SYMLINK_PREFIX: &str = "mur_agent_";
/// Root of the `Default` chain: outside any real path, so it never fires.
const INERT_ROOT: &str = "/nonexistent-launch-chain-root";

/// The capture stores under `<mur_home>`: recordings of what was done, kept
/// verbatim (see `protects_capture_store`). The one list of their names —
/// the predicate, `credential_paths()` and the SBPL write-deny all read it.
pub const CAPTURE_STORES: [&str; 5] = ["queue", "session", "conversations", "telemetry", "traces"];

/// `<mur_home>/publisher`: the skill publisher identity (#1011). Not an agent
/// home, so `private_key_dir` leaves its `identity.key` here.
const PUBLISHER_DIR: &str = "publisher";
/// The private half of an `AgentIdentity`, in whatever directory it lives.
const IDENTITY_KEY: &str = "identity.key";

#[derive(Clone, Debug)]
pub struct LaunchChain {
    mur_home: PathBuf,
    agent_home: PathBuf,
    bin_dir: PathBuf,
    autostart: Vec<PathBuf>,
    /// The user's home, for [`mur_common::agent::DEFAULT_DENY_PATHS`].
    user_home: PathBuf,
}

impl LaunchChain {
    /// Derive the protected set for the agent rooted at `agent_home`.
    pub fn new(agent_home: &Path) -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        let bin_dir = std::env::var_os("MUR_AGENT_BIN_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/bin"));
        Self::build(agent_home, &bin_dir, &home)
    }

    /// Construct with explicit roots. Tests use this; `new` is the real path.
    pub fn for_test(agent_home: &Path, bin_dir: &Path, home: &Path) -> Self {
        Self::build(agent_home, bin_dir, home)
    }

    /// A chain rooted outside any real path, so it never fires.
    ///
    /// For tests that exercise something else and just need to construct a
    /// tool. Tests that exercise the chain build their own with `for_test`.
    #[cfg(test)]
    pub fn inert() -> Self {
        Self::default()
    }

    fn build(agent_home: &Path, bin_dir: &Path, home: &Path) -> Self {
        // `<mur_home>/agents/<name>` — the same derivation policy/build.rs uses for
        // the channels and open-items force-grants.
        let mur_home = agent_home
            .parent()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| agent_home.to_path_buf());
        Self {
            mur_home,
            agent_home: agent_home.to_path_buf(),
            bin_dir: bin_dir.to_path_buf(),
            autostart: autostart_dirs(home),
            user_home: home.to_path_buf(),
        }
    }

    pub fn agent_self_home(&self) -> &Path {
        &self.agent_home
    }

    /// True for the never-firing chain of `Default` — no real agent home.
    pub fn is_inert(&self) -> bool {
        self.mur_home.starts_with(INERT_ROOT)
    }

    /// Why `path` may never be written, or `None` if it is not in the set.
    ///
    /// Returns the reason rather than a bare `bool` because the tool gate is
    /// the only layer that can explain itself — the kernel returns an EPERM
    /// that reads identically to "not granted".
    pub fn protects_write(&self, path: &Path) -> Option<&'static str> {
        if self.is_other_agent(path) {
            return Some(
                "another agent's directory — its profile.yaml is that agent's \
                 entitlements and its identity.key is that agent's signing authority",
            );
        }
        if path.starts_with(self.mur_home.join(mur_common::entitlements_pin::PINS_DIR)) {
            return Some(
                "the entitlement pins — the supervisor trusts a profile's \
                 entitlements only if they match the pin, so rewriting it grants \
                 whatever the profile says",
            );
        }
        if path.starts_with(mur_common::git_push::broker_dir(&self.mur_home)) {
            return Some(
                "the git push broker's directory — its registry maps each repo_id to the \
                 repo a push is built from, so rewriting it redirects a human-approved push",
            );
        }
        if self.is_launch_artifact(path) {
            return Some(
                "MUR's runtime binary or a per-agent symlink — exec'd before \
                 the sandbox applies, so replacing it escapes every sandbox",
            );
        }
        if self.autostart.iter().any(|d| path.starts_with(d)) {
            return Some("an OS autostart directory — entries here run outside any sandbox");
        }
        if self.commander_public_keys().iter().any(|k| path == k) {
            return Some(
                "a pinned commander public key — directives signed by it are \
                 obeyed, so writing it lets an agent sign its own governance",
            );
        }
        if let Some(reason) = self.protects_credential(path) {
            return Some(reason);
        }
        None
    }

    /// Why `path` may never be read.
    ///
    /// Two families, both "reading this is equivalent to holding a credential":
    /// a sibling's signing key, and the user's own credential store. Neither
    /// is expressible as an entitlement — an agent that could be granted them
    /// could impersonate another agent or the user, so no grant may authorise
    /// it and the gate sits before the allow/deny lists.
    pub fn protects_read(&self, path: &Path) -> Option<&'static str> {
        if self.is_other_agent(path) && path.file_name().is_some_and(|n| n == "identity.key") {
            return Some(
                "another agent's signing key — reading it is enough to forge \
                 that agent's signed channel events",
            );
        }
        // Issue #007: the agent's OWN identity.key stays read-protected for the
        // same reason a sibling's does — holding it is signing authority. Its
        // own profile.yaml is deliberately NOT here: #712 is a *write* rule
        // (self-edit + self-restart defeats the seal), and extending it to
        // reads only ever cost the agent the ability to answer "what am I
        // allowed to do?" while stopping no escalation — the sibling profile
        // next door was readable the whole time.
        if path.starts_with(&self.agent_home)
            && path.file_name().is_some_and(|n| n == "identity.key")
        {
            return Some(
                "this agent's own signing key — reading it is enough to forge \
                 its signed channel events",
            );
        }
        if let Some(reason) = self.protects_credential(path) {
            return Some(reason);
        }
        None
    }

    /// The user's credentials, which no agent has a reason to read.
    ///
    /// `secrets/` holds provider API keys and the commander token in plain
    /// text; `auth.json` holds the account access + refresh tokens; the
    /// top-level `identity.key` is the host key. An agent reaches its model
    /// through the runtime's own client, which resolves credentials before the
    /// sandbox is sealed — it never needs the files.
    ///
    /// This is deliberately BOTH read and write: writing `secrets/` swaps the
    /// key an unrelated agent will use, and writing `auth.json` is a session
    /// takeover.
    fn protects_credential(&self, path: &Path) -> Option<&'static str> {
        // Every agent's private key (#850 option (c)). A subtree test, so a
        // key created after the policy was built is covered — which the
        // `sibling_signing_keys` enumeration this replaced could not do.
        if path.starts_with(self.mur_home.join("keys")) {
            return Some(
                "an agent's private signing key — reading it is enough to forge \
                 that agent's signed channel events",
            );
        }
        if path.starts_with(self.mur_home.join("secrets")) {
            return Some(
                "MUR's credential store — provider API keys and the commander \
                 token, which no agent needs and any agent could exfiltrate",
            );
        }
        if path == self.mur_home.join("auth.json") {
            return Some(
                "the account's access and refresh tokens — reading them is a \
                 session takeover, not a file read",
            );
        }
        if path == self.mur_home.join("identity.key") {
            return Some("the host signing key");
        }
        // `identity.key` is the live key (`AgentIdentity::load` on the
        // commander dir); `signing.key` is the legacy name older installs keep.
        if self.commander_private_keys().iter().any(|k| path == k) {
            return Some("the commander's signing key — governance authority");
        }
        if path == self.publisher_private_key() {
            return Some(
                "the skill publisher's signing key — holding it forges the \
                 signature on every skill this user publishes",
            );
        }
        if path == self.mur_home.join("mobile").join("pair-token") {
            return Some("the phone pairing token");
        }
        // `.env` under `<mur_home>` is a credential file by convention, and
        // `commander/.env` really does hold SLACK_BOT_TOKEN,
        // SLACK_SIGNING_SECRET, SLACK_APP_TOKEN and ANTHROPIC_API_KEY. Denying
        // `commander/signing.key` alone missed it — the third time this list
        // proved incomplete.
        if path.file_name().is_some_and(|n| n == ".env") && path.starts_with(&self.mur_home) {
            return Some("a .env file under MUR's home — credentials by convention");
        }
        if path == self.mur_home.join("runtime").join("vlc.json") {
            return Some("the VLC control password");
        }
        if path.starts_with(self.mur_home.join("actions-runner")) {
            return Some(
                "a self-hosted CI runner's credentials — they authenticate as \
                 that runner against the whole repository host",
            );
        }
        if let Some(reason) = self.protects_capture_store(path) {
            return Some(reason);
        }
        if self
            .user_credential_dirs()
            .iter()
            .any(|d| path.starts_with(d))
        {
            return Some(
                "the user's own credentials (SSH keys, cloud credentials, GPG \
                 keyring) — holding them is acting as the user on every host \
                 they unlock",
            );
        }
        None
    }

    /// [`mur_common::agent::DEFAULT_DENY_PATHS`] resolved against `$HOME`.
    ///
    /// The same list new profiles are written with, enforced here as well so
    /// an install whose profile predates it — or whose deny list was emptied —
    /// is still covered. Like the rest of this chain it sits before the
    /// allow/deny lists: no grant reaches them.
    fn user_credential_dirs(&self) -> Vec<PathBuf> {
        mur_common::agent::DEFAULT_DENY_PATHS
            .iter()
            .map(|d| self.user_home.join(d.trim_start_matches("~/")))
            .collect()
    }

    /// The capture stores, which record what was DONE rather than what was
    /// configured — and record it verbatim.
    ///
    /// `queue/events.jsonl` is the CLI hook pipeline's event log: every tool
    /// call, including shell command lines as typed. It is not redacted —
    /// `inject::queue::enqueue_to` serialises the event and appends it, and
    /// nothing in `capture/` filters first. The redaction chokepoint that does
    /// exist (`telemetry_writer::redact_envelope`, B0 rule 9) is a DIFFERENT
    /// writer, on the runtime's own telemetry path, and never sees this file.
    ///
    /// So any credential that ever appeared on a command line is in here in
    /// plain text. A 200 MB sample of a real 934 MB queue matched 21 lines of
    /// `sk-ant-` shape, 12 of `ghp_`, 8 of `AKIA`, and 180 Authorization
    /// headers.
    ///
    /// `session/`, `conversations/`, `telemetry/` and `traces/` are the same
    /// class: a recording of the user's work, not state an agent operates on.
    ///
    /// Nothing in the agent runtime reads any of them.
    fn protects_capture_store(&self, path: &Path) -> Option<&'static str> {
        for s in CAPTURE_STORES {
            if path.starts_with(self.mur_home.join(s)) {
                return Some(
                    "a capture store — an unredacted verbatim record of every \
                     command run, which no agent reads and any agent could mine",
                );
            }
        }
        None
    }

    /// `<mur_home>/<store>` for each of [`CAPTURE_STORES`].
    pub fn capture_store_paths(&self) -> Vec<PathBuf> {
        CAPTURE_STORES
            .iter()
            .map(|s| self.mur_home.join(s))
            .collect()
    }

    fn commander_dir(&self) -> PathBuf {
        self.mur_home.join(mur_common::commander::COMMANDER_DIR)
    }

    /// The live commander key and its legacy name.
    fn commander_private_keys(&self) -> [PathBuf; 2] {
        let d = self.commander_dir();
        [d.join(IDENTITY_KEY), d.join("signing.key")]
    }

    /// The pinned commander keys. Readable (the fleet loop verifies
    /// directives against them), never writable.
    pub fn commander_public_keys(&self) -> [PathBuf; 2] {
        let d = self.commander_dir();
        [
            d.join(mur_common::commander::COMMANDER_PUB),
            d.join(mur_common::commander::COMMANDER_PREV_PUB),
        ]
    }

    fn publisher_private_key(&self) -> PathBuf {
        self.mur_home.join(PUBLISHER_DIR).join(IDENTITY_KEY)
    }

    /// The credential files and subtrees outside the capture stores: what
    /// `credential_paths()` denies for reading because holding it is holding
    /// a credential. Also exactly the rows `write_protected_paths()` adds on
    /// top of `deny_paths()`, minus the commander public keys.
    fn credential_files(&self) -> Vec<PathBuf> {
        let mut out = vec![
            // Every agent's private key, as ONE subtree (#850 option (c)).
            // This replaces the `sibling_signing_keys()` enumeration, and with
            // it the after-seal gap: a key created here after the policy was
            // sealed is still inside the denied subpath, whereas an enumerated
            // list could never name it.
            //
            // No re-allow for the agent's own key, and none is needed: the
            // runtime loads its identity in `supervisor::entrypoint` before
            // `supervisor::seal` applies the sandbox, so nothing reads a
            // private key after the seal.
            self.mur_home.join("keys"),
            self.mur_home.join("secrets"),
            self.mur_home.join("auth.json"),
            self.mur_home.join(IDENTITY_KEY),
        ];
        out.extend(self.commander_private_keys());
        out.extend([
            self.commander_dir().join(".env"),
            self.publisher_private_key(),
            self.mur_home.join("mobile").join("pair-token"),
            self.mur_home.join("actions-runner"),
            self.mur_home.join("runtime").join("vlc.json"),
        ]);
        out.extend(self.user_credential_dirs());
        out
    }

    /// Paths no write grant may reach, as concrete paths for backends that
    /// need a list (Landlock partition, SBPL write-deny). Write integrity,
    /// not privacy: every entry is here because WRITING it gains authority.
    ///
    /// `deny_paths()` (the launch chain), the credential files, and the
    /// pinned commander public keys. Deliberately NOT the capture stores:
    /// writing a recording grants nothing, and the fleet runner's own
    /// `conversations/` grant must survive the Landlock partition. Each
    /// backend still write-denies them on its own terms.
    pub fn write_protected_paths(&self) -> Vec<PathBuf> {
        let mut out = self.deny_paths();
        out.extend(self.credential_files());
        out.extend(self.commander_public_keys());
        out
    }

    /// Concrete paths for backends that need a list rather than a predicate
    /// (SBPL). `<mur_home>/agents` is included as a whole; the caller is
    /// responsible for re-allowing `agent_self_home()` after it.
    pub fn deny_paths(&self) -> Vec<PathBuf> {
        let mut out = vec![
            self.mur_home.join("agents"),
            self.mur_home.join(mur_common::entitlements_pin::PINS_DIR),
            // Premise 1 of the git-push registry decision: the broker dir is
            // daemon-written and agent-read-only. In this list, a Landlock write
            // grant overlapping it is dropped whole and SBPL denies it.
            mur_common::git_push::broker_dir(&self.mur_home),
        ];
        out.push(self.bin_dir.join(RUNTIME_BINARY));
        out.extend(self.existing_agent_symlinks());
        out.extend(self.autostart.iter().cloned());
        out
    }

    /// Under `<mur_home>/agents` but not under this agent's own home.
    fn is_other_agent(&self, path: &Path) -> bool {
        path.starts_with(self.mur_home.join("agents")) && !path.starts_with(&self.agent_home)
    }

    fn is_launch_artifact(&self, path: &Path) -> bool {
        if path.parent() != Some(self.bin_dir.as_path()) {
            return false;
        }
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == RUNTIME_BINARY || n.starts_with(AGENT_SYMLINK_PREFIX))
    }

    /// Symlinks present now. One created later is not listed, which is
    /// acceptable: a new symlink only matters if something starts it, and
    /// that needs a profile (denied) or an autostart entry (denied) or a human.
    /// The credential paths, as concrete paths for the SBPL emitter. Same set
    /// `protects_credential` refuses at grant time; this is the kernel side,
    /// so a read that never goes through the file tools (a spawned process, an
    /// MCP server, a library doing raw I/O) is stopped too.
    ///
    /// Unlike sibling keys these are FIXED paths, so there is no enumeration
    /// and no after-seal gap: a `secrets/` file created later is still under
    /// the denied subtree.
    pub fn credential_paths(&self) -> Vec<PathBuf> {
        let mut out = self.credential_files();
        out.extend(self.capture_store_paths());
        out
    }

    /// Split write grants into those the sandbox can install and those it must
    /// drop whole.
    ///
    /// Lives here rather than in `sandbox::linux` because it is pure launch-chain
    /// path logic that every platform needs — the module comment there already
    /// said as much ("shared with sandbox::policy on every platform; only the apply
    /// path inside is linux-gated"). Being private to that module is also why
    /// `mur agent doctor` could not report what it computes.
    ///
    /// The agent's own home is exempt: on macOS the SBPL deny of the agents tree
    /// is followed by a re-allow of exactly this directory, and Landlock installs
    /// it as-is (it contains nothing protected — the own profile/identity
    /// self-protection is macOS tier 3 only). Without the exemption the symmetric
    /// overlap test below would also catch the `<mur_home>/agents/<self>`
    /// force-grant and Linux agents would lose their own home.
    ///
    /// Compared under every spelling, not just as written: the policy builder
    /// keeps a symlinked grant AND its resolved target, and Landlock resolves
    /// the link when the rule is installed. A lexical test drops the target but
    /// keeps `/work/link -> <mur_home>/secrets`, handing the kernel the very
    /// subtree this list exists to close. See [`Self::partition_by_spelling`].
    pub fn partition_grants(&self, grants: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
        self.partition_by_spelling(grants, &self.write_protected_paths())
    }

    /// The shared body of both partitions. A grant is kept only if NO
    /// spelling of it (as written, canonical) overlaps ANY spelling of a
    /// protected path. The own-home exemption also needs every spelling of the
    /// grant to stay under some spelling of the own home, so an alias placed
    /// inside the agent's home cannot carry a protected target past it.
    ///
    /// A path that does not resolve keeps only its written form; the builder
    /// already drops grants missing from disk, and a protected path that does
    /// not exist yet is still matched as written.
    fn partition_by_spelling(
        &self,
        grants: &[PathBuf],
        protected: &[PathBuf],
    ) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let protected: Vec<PathBuf> = protected.iter().flat_map(|p| spellings(p)).collect();
        let own = spellings(self.agent_self_home());
        grants.iter().cloned().partition(|g| {
            let forms = spellings(g);
            if forms.iter().all(|f| own.iter().any(|o| f.starts_with(o))) {
                return true;
            }
            !forms.iter().any(|f| {
                protected
                    .iter()
                    .any(|p| p.starts_with(f) || f.starts_with(p))
            })
        })
    }
    /// The paths no read grant may reach: the user's credential store and
    /// sibling signing keys.
    ///
    /// Deliberately NOT `deny_paths()`. That contains the whole `agents/`
    /// subtree, and a blanket read-deny there fail-closes every multi-agent
    /// channel — verifying a peer's signed events reads `identity.pub` and
    /// `rotations.jsonl` from that peer's home (audit §2). This set is exactly
    /// what macOS emits as `deny file-read*`, so the two backends refuse the
    /// same reads instead of diverging.
    ///
    /// No after-seal gap any more: `keys/` is a fixed subtree in
    /// `credential_paths()`, so a private key created after the policy sealed
    /// is inside it by construction. That is what the move bought.
    fn read_protected_paths(&self) -> Vec<PathBuf> {
        // Just the credential paths now: `keys/` is one of them, so every
        // agent's private key is covered as a subtree with no enumeration and
        // no after-seal gap (#850 option (c) step 3).
        self.credential_paths()
    }

    /// Split READ grants into those the sandbox can install and those it must
    /// drop whole — the counterpart of [`Self::partition_grants`].
    ///
    /// Same Landlock reasoning as the write side: a pure allow-list has no deny
    /// rule, so a protected path inside a grant cannot be carved out and the
    /// grant is dropped entire, fail-closed. Without this a broad `fs_read`
    /// (`~/.mur`, or `~` itself) hands an agent the credential store outright,
    /// while the identical write grant is refused — the divergence #850 names.
    ///
    /// The agent's own home is exempt, as on the write side: it must read its
    /// own profile and state. macOS tier-3 self-protection (own `identity.key`
    /// / `profile.yaml`) is emitted separately and is unaffected.
    pub fn partition_read_grants(&self, grants: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
        self.partition_by_spelling(grants, &self.read_protected_paths())
    }
    fn existing_agent_symlinks(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.bin_dir) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with(AGENT_SYMLINK_PREFIX))
            })
            .map(|e| e.path())
            .collect()
    }
}

/// `p` as written, plus its canonical form when that differs. Symlinks in
/// any component are resolved, so `/work/link/sub` and the target's `/sub`
/// are both returned.
fn spellings(p: &Path) -> Vec<PathBuf> {
    let mut v = vec![p.to_path_buf()];
    if let Ok(real) = std::fs::canonicalize(p)
        && real != p
    {
        v.push(real);
    }
    v
}

impl Default for LaunchChain {
    /// Rooted outside any real path, so it never fires. Only reachable via
    /// `SandboxPolicy::default()` (tests and policy-less contexts); every
    /// real policy is built by `from_entitlements`, which constructs the
    /// actual chain from `agent_home`.
    fn default() -> Self {
        let root = Path::new(INERT_ROOT);
        Self::build(&root.join("agents/none"), &root.join("bin"), root)
    }
}

#[cfg(target_os = "macos")]
fn autostart_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join("Library/LaunchAgents"),
        home.join("Library/LaunchDaemons"),
    ]
}

#[cfg(target_os = "linux")]
fn autostart_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".config/systemd/user"),
        home.join(".config/autostart"),
    ]
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn autostart_dirs(_home: &Path) -> Vec<PathBuf> {
    Vec::new()
}

/// A grant root so broad that granting it is equivalent to no sandbox.
///
/// Unifies the judgement previously duplicated in `access.rs::is_overbroad_root`
/// (cwd consent) and `policy/mod.rs::is_guarded_prefix` (spawn prefixes). Those two
/// disagreed: one knew about `/usr` and `/opt`, the other about depth. This is
/// the union.
pub fn is_overbroad_grant_root(path: &Path, home: &Path) -> bool {
    if path == Path::new("/")
        || path == home
        || path == Path::new("/usr")
        || path == Path::new("/opt")
        || path == Path::new("/opt/homebrew")
    {
        return true;
    }
    if let Ok(rest) = path.strip_prefix("/Volumes") {
        return rest.components().count() <= 1;
    }
    path.components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .count()
        < 2
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod write_protected_tests;
