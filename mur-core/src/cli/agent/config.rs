//! Configuration subcommand enums for `mur agent` (perm, skill, mcp, addon, prompt).

use clap::Subcommand;

#[derive(Subcommand)]
pub enum AgentPermAction {
    /// Print the entitlements block (or one section, e.g. "network")
    Show {
        name: String,
        section: Option<String>,
    },
    /// Set a mode (currently only `network.outbound <restricted|unrestricted|proxy_only|off>`)
    SetMode {
        name: String,
        key: String,
        value: String,
    },
    /// Allow an outbound host glob
    AllowHost {
        name: String,
        glob: String,
        /// Approve without a prompt. Required when there is no terminal
        /// (scripts, agents) — the grant is refused otherwise.
        #[arg(long)]
        yes: bool,
    },
    /// Deny an outbound host glob
    DenyHost { name: String, glob: String },
    /// Print the outbound host allow / deny lists
    ListHosts { name: String },
    /// Allow an extra outbound TCP port (on top of 80/443/8080/8443)
    AllowPort { name: String, port: u16 },
    /// Remove an extra outbound TCP port grant
    DenyPort { name: String, port: u16 },
    /// Print the outbound TCP ports this agent may dial
    ListPorts { name: String },
    /// Print the filesystem grants and what the sandbox actually installed
    ListPaths { name: String },
    /// Allow filesystem read on a path
    AllowRead { name: String, path: String },
    /// Allow filesystem write on a path
    AllowWrite { name: String, path: String },
    /// Deny filesystem access on a path
    DenyPath { name: String, path: String },
    /// Take a grant back: drop a path from the read, write, or deny list
    /// (deny-path ADDS to the deny list; this is the only way to undo one)
    RemovePath {
        name: String,
        /// Which list: read, write, or deny
        list: String,
        path: String,
    },
    /// Add a binary to the spawn allowlist
    AllowSpawn { name: String, binary: String },
    /// Remove a binary from the spawn allowlist
    DenySpawn { name: String, binary: String },
    /// Allow exec of everything under a directory — the build lane, for
    /// toolchains that compile and then run their own binaries (cargo build
    /// scripts, proc macros, test executables). Grant a build-output
    /// directory, never a source tree or a home directory
    AllowSpawnDir { name: String, dir: String },
    /// Remove a directory from the build lane
    DenySpawnDir { name: String, dir: String },
    /// Set a numeric resource limit (memory_mb, file_descriptors, processes)
    SetLimit {
        name: String,
        key: String,
        value: u64,
    },
    /// Set tool policy: allow | ask | deny
    ToolAllow { name: String, pattern: String },
    /// Set tool policy to ask (HITL prompt before execution)
    ToolAsk { name: String, pattern: String },
    /// Deny a tool pattern entirely
    ToolDeny { name: String, pattern: String },
    /// Remove a tool rule
    ToolClear { name: String, pattern: String },
    /// List all tool rules
    ToolList { name: String },
    /// Accept the entitlements now in profile.yaml as trusted (#712). Needed
    /// after a hand edit of `entitlements:`; the agent refuses to start on
    /// entitlements that changed outside MUR until you reseal
    Reseal { name: String },
    /// Layer-3 tier check: a model reads each `bash` call the static
    /// classifier tiers `write` and may make it ask. It can never make a
    /// call skip a prompt. `--off` turns it off
    IntentCheck {
        name: String,
        /// `~/.mur/models.yaml` ref. A cloud model receives every checked
        /// command; a local model keeps commands on this machine
        #[arg(required_unless_present = "off", conflicts_with = "off")]
        model_ref: Option<String>,
        /// Wait this long for the model before keeping the static tier
        #[arg(long)]
        timeout_ms: Option<u64>,
        #[arg(long)]
        off: bool,
    },
}

#[derive(Subcommand)]
pub enum AgentSkillAction {
    /// List attached skill ids
    List { name: String },
    /// Validate a skill and install it into the agent, then register it.
    /// The source is parsed (`.yaml`/`.yml`, or `.md` which is accepted and
    /// converted to canonical YAML), schema-validated and security-scanned,
    /// then written into a per-skill subdir as `skills/<name>/skill.yaml`
    /// (name taken from the manifest). Sources that aren't valid skill
    /// manifests are rejected rather than stored as dead files.
    Add {
        name: String,
        /// Path to a skill source (`.yaml`/`.yml` or `.md`). Stored id is
        /// `skills/<manifest-name>`.
        source: String,
    },
    /// Remove a skill entry; deletes the backing subdir if orphaned.
    /// `skill_id` may be the full id (`skills/foo`), the basename (`foo`),
    /// or the basename without extension.
    Remove { name: String, skill_id: String },
    /// Migrate a legacy path-style skill ref (`profile.skills`) into a
    /// structured `installed_skills` card, in place. The backing `skill.yaml`
    /// already lives in the loadable layout, so nothing moves on disk — only
    /// the profile pointer. `skill_id` may be the full id or the basename.
    Convert { name: String, skill_id: String },
    /// Print the canonical contents of an installed skill.
    /// `skill_id` may be the full id (`skills/foo`), the basename (`foo`),
    /// or the basename without extension.
    Show { name: String, skill_id: String },
    /// Enable a previously disabled skill for this agent (clears the denylist
    /// entry). Non-destructive — the skill is never re-installed or removed.
    Enable { name: String, skill_id: String },
    /// Disable a skill for this agent WITHOUT uninstalling it. The skill's
    /// files + stats are kept; it simply stops injecting. Applies on the
    /// agent's next restart.
    Disable { name: String, skill_id: String },
    /// Fetch a skill from a remote URL, security-scan it, and install it onto
    /// the agent. The URL must use `https` (or `http` on localhost for dev).
    /// If the scan finds blocking issues, the install is refused unless `--yes`
    /// is passed. Changes take effect on the agent's next restart.
    AddUrl {
        /// Agent name
        name: String,
        /// Remote skill URL (`https://…/skill.yaml` or `…/skill.md`)
        url: String,
        /// Accept and install despite blocking security-scan findings
        #[arg(long)]
        yes: bool,
    },
    /// Install a skill from the MUR skill registry onto an agent
    /// (e.g. `mur agent skill registry-add myagent rust-testing`).
    /// Verifies content hash and publisher signature before installing
    /// (fail-closed); installs at Sandboxed trust level. Use `--yes` to
    /// accept an unsigned/unhashed skill (a hash mismatch or invalid
    /// signature is refused unconditionally — `--yes` does not override it).
    RegistryAdd {
        /// Agent name
        name: String,
        /// Registry skill name
        skill: String,
        /// Specific version to install (defaults to latest)
        #[arg(long)]
        version: Option<String>,
        /// Accept an unsigned/unhashed skill (hash mismatch / invalid signature
        /// is refused unconditionally)
        #[arg(long)]
        yes: bool,
    },
    /// Install every registry skill recommended for a fleet role onto an
    /// agent (e.g. `install-pack coder`), skipping ones already installed.
    InstallPack {
        /// Agent name
        #[arg(long)]
        agent: String,
        /// Role (e.g. concierge, pm, coder, qa, repo)
        role: String,
        /// Accept unsigned/unhashed skills (hash mismatch / invalid signature
        /// is refused unconditionally)
        #[arg(long)]
        yes: bool,
    },
    /// Search the MUR skill registry (for skills installable onto an agent).
    Search {
        /// Agent name
        name: String,
        /// Search query
        query: String,
    },
    /// Trust a skill publisher key (TOFU) — adds it to `~/.mur/trust/publishers.yaml`
    /// so future installs signed by that key are treated as Trusted.
    /// The fingerprint is shown in the install consent screen for
    /// unknown-but-verified signers.
    TrustPublisher {
        /// Publisher key fingerprint (e.g. `ed25519-abcd1234`)
        key_fp: String,
        /// Friendly name recorded alongside the key
        #[arg(long)]
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum AgentMcpAction {
    /// List MCP servers attached to an agent
    List { name: String },
    /// Add an MCP server entry and sync its command into the spawn allowlist
    Add {
        name: String,
        server_id: String,
        #[arg(long)]
        command: String,
        /// A single argument for the MCP command (repeatable). Values may
        /// start with `-`/`--`, e.g. `--arg --engine` or `--arg=--engine`.
        #[arg(long = "arg", allow_hyphen_values = true)]
        args: Vec<String>,
        /// A directory this server writes state into at runtime (repeatable).
        /// Created if missing, then granted read+write. Many MCP servers put
        /// config, logs or a device id under $HOME on first launch and exit
        /// before answering `initialize` when the sandbox denies it.
        #[arg(long = "state-path")]
        state_paths: Vec<String>,
        /// Skip the y/N install confirmation prompt (B0 rule 6 / M9.2).
        /// Use for scripted / non-interactive installs.
        #[arg(long)]
        force: bool,
        /// Install without spawning the server to check it works.
        ///
        /// The install normally runs `initialize` + `tools/list` under this
        /// agent's real sandbox, and refuses to write an entry that fails —
        /// an entry that cannot start is an agent that cannot start. Use this
        /// for servers that legitimately cannot be probed (side-effecting
        /// init, hardware, a login flow), or raise `MUR_MCP_PROBE_TIMEOUT_S`
        /// first if the server is merely slow to boot.
        #[arg(long = "no-probe")]
        no_probe: bool,
        /// Publisher name shown in the install prompt and pinned into
        /// the profile for audit (e.g. "Anthropic" or "@alice").
        #[arg(long = "publisher-name")]
        publisher_name: Option<String>,
        /// Publisher homepage URL (display-only).
        #[arg(long = "publisher-homepage")]
        publisher_homepage: Option<String>,
        /// Publisher registry coordinate (display-only,
        /// e.g. "@anthropic-mcp/weather@1.2.3").
        #[arg(long = "publisher-registry-id")]
        publisher_registry_id: Option<String>,
    },
    /// Remove an MCP server entry by id
    Remove { name: String, server_id: String },
    /// Rename an MCP server entry in place
    Rename {
        name: String,
        old: String,
        new: String,
    },
    /// Show pinned vs current state for one (or all) MCP entries.
    /// Exit codes (B0 rule 6 / M9.4): 0 clean, 1 binary drift, 4
    /// unpinned (pre-M9), 5 binary missing. Reports the worst status
    /// across all servers when `--server` is omitted.
    Inspect {
        name: String,
        /// Limit to a single MCP server entry; otherwise iterate all.
        #[arg(long = "server")]
        server: Option<String>,
        /// Spawn the MCP and verify `tools/list` against the pinned
        /// description hash (B0 rule 6 / M9.3.5). Off by default —
        /// inspect stays fast; pass `--probe` for the active scan.
        #[arg(long)]
        probe: bool,
        /// Re-install a vendored entry from its pinned lockfile and diff the
        /// result against what's on disk. The only check here whose reference
        /// value comes from the registry rather than this machine, so it sees
        /// a locally-edited tree even when the recorded hash was edited to
        /// match. Costs a full reinstall.
        #[arg(long)]
        deep: bool,
    },
    /// Re-compute and persist the install-time binary pin (and
    /// optionally refresh publisher metadata). Used after reviewing
    /// drift surfaced by `mur agent mcp inspect`.
    Pin {
        name: String,
        server_id: String,
        /// Skip the y/N re-approval prompt.
        #[arg(long)]
        force: bool,
        /// Skip the live MCP probe (M9.3.5). Use for MCPs that can't
        /// be cleanly probed (slow boot, side-effecting init, etc.).
        /// `description_hash` stays at its previous value.
        #[arg(long = "no-probe")]
        no_probe: bool,
        #[arg(long = "publisher-name")]
        publisher_name: Option<String>,
        #[arg(long = "publisher-homepage")]
        publisher_homepage: Option<String>,
        #[arg(long = "publisher-registry-id")]
        publisher_registry_id: Option<String>,
    },
    /// Install a package-runner MCP server (`npx @scope/pkg`) into a directory
    /// MUR owns and launch it directly, so its contents can be verified.
    ///
    /// A runner resolves the package at every spawn, which pins nothing: the
    /// binary hash covers `npx`, not the server. Vendoring installs the exact
    /// version under `~/.mur/mcp-packages/`, repoints the entry at the
    /// installed script, and records the lockfile fingerprint that B0 rule 6
    /// then enforces at startup. Applies on the agent's next restart.
    Vendor {
        name: String,
        server_id: String,
        /// Version to install. Defaults to the version already pinned in the
        /// entry's args, or the registry's current one.
        #[arg(long)]
        version: Option<String>,
        /// Skip the y/N approval prompt.
        #[arg(long)]
        force: bool,
    },
    /// Enable a previously disabled MCP server for this agent.
    Enable { name: String, server_id: String },
    /// Disable an MCP server for this agent WITHOUT removing it. The entry +
    /// its pin stay in the profile; it simply stops spawning. Applies on the
    /// agent's next restart.
    Disable { name: String, server_id: String },
    /// Scope an MCP server's outbound network to a host allowlist (routed
    /// through the runtime egress proxy). No `--allow-host` and no `--off`
    /// clears the policy (inherit the agent's). Applies on the agent's restart.
    SetNetwork {
        name: String,
        server_id: String,
        /// Allowed host (repeatable), e.g. `--allow-host example.com --allow-host '*.api.example.com'`.
        #[arg(long = "allow-host")]
        allow_hosts: Vec<String>,
        /// Also grant this agent an extra outbound TCP port (repeatable).
        /// Required when an `--allow-host` names a non-web port
        /// (`host:9000`): the egress proxy runs inside the agent's sandbox,
        /// so the host grant alone cannot reach it. Opens the port to ANY
        /// host for the whole agent — same grant as `perm allow-port`.
        #[arg(long = "allow-port", conflicts_with = "off")]
        allow_ports: Vec<u16>,
        /// Denied host (repeatable). With `--broad-audited`, these are the
        /// deny-overlay hosts; ignored otherwise.
        #[arg(long = "deny-host")]
        deny_hosts: Vec<String>,
        /// Deny this server all outbound network.
        #[arg(long)]
        off: bool,
        /// Grant allow-ALL-except-`--deny-host` egress, routed through the
        /// audited proxy. Permission-required: requires operator consent
        /// (prompts unless `--yes`) and records who authorized it and when.
        #[arg(long = "broad-audited", conflicts_with = "off")]
        broad_audited: bool,
        /// Approve widening grants without a prompt. Required when there is no
        /// terminal (scripts, agents) — the grant is refused otherwise.
        #[arg(long)]
        yes: bool,
    },
    /// Scan other installed tools (Claude Desktop/Code, Cursor, VS Code,
    /// Windsurf, Antigravity, Gemini CLI, Codex) for MCP servers you can import.
    Discover,
    /// Search the official MCP Registry (registry.modelcontextprotocol.io).
    Search { query: String },
    /// Install a server from the MCP Registry onto an agent, by its registry
    /// name (e.g. `mur agent mcp registry-add rustsmith com.example/fs`).
    RegistryAdd {
        name: String,
        server: String,
        /// Skip the y/N install confirmation prompt (B0 rule 6 / M9.2).
        /// Use for scripted / non-interactive installs.
        #[arg(long)]
        force: bool,
    },
    /// Add a remote (Streamable HTTP) MCP server by URL.
    AddRemote {
        /// Agent name
        name: String,
        /// Server id (local nickname in the profile)
        server_name: String,
        /// HTTPS base URL of the remote MCP server
        url: String,
        /// Bearer token from an env var (e.g. `MY_TOKEN`).
        #[arg(long)]
        bearer_env: Option<String>,
        /// Bearer token from the keychain (`service/account`).
        #[arg(long)]
        bearer_keychain: Option<String>,
    },
    /// Run the OAuth 2.1 authorization-code + PKCE flow for a remote MCP
    /// server (added via `add-remote`) and store the resulting tokens.
    Login {
        /// Agent name
        name: String,
        /// Server id (the local nickname used when the server was added)
        server: String,
    },
}

#[derive(Subcommand)]
pub enum AgentAddonAction {
    /// Import a local Claude plugin directory into this agent (fail-closed: disabled by default).
    Import {
        /// Agent name
        name: String,
        /// Local plugin directory, or a git source to install from the network:
        /// `owner/repo` (GitHub), an https/ssh git URL, or any `*.git`. Git
        /// sources are shallow-cloned into `~/.mur/cache/addons/` then imported.
        plugin_dir: String,
        /// When the source is a marketplace (`.claude-plugin/marketplace.json`
        /// indexing many plugins), the plugin to install. Omit to list them.
        #[arg(long)]
        plugin: Option<String>,
        /// Override security-scan blocks (review the skill first)
        #[arg(long)]
        force: bool,
    },
    /// List imported add-ons for an agent
    List {
        /// Agent name
        name: String,
    },
    /// Enable an add-on for an agent (the only verb that sets enabled=true)
    Enable {
        /// Agent name
        name: String,
        /// Add-on id (from `mur agent addon list`)
        addon_id: String,
    },
    /// Disable an add-on without removing it
    Disable {
        /// Agent name
        name: String,
        /// Add-on id (from `mur agent addon list`)
        addon_id: String,
    },
    /// Remove an add-on: deletes skill dirs + MCP entries + AddonRef (non-destructive to agents)
    Remove {
        /// Agent name
        name: String,
        /// Add-on id (from `mur agent addon list`)
        addon_id: String,
    },
    /// Kill-switch: disable ALL add-ons for an agent at once
    DisableAll {
        /// Agent name
        name: String,
    },
    /// Re-fetch and re-apply an add-on from its recorded source
    Reimport {
        /// Agent name
        name: String,
        /// Add-on id (from `mur agent addon list`)
        addon_id: String,
        /// Override the fetch source (a path or owner/repo)
        #[arg(long)]
        from: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum AgentPromptAction {
    /// Print the agent's sys_prompt.md contents to stdout
    Show { name: String },
    /// Open $EDITOR on sys_prompt.md
    Edit { name: String },
    /// Write sys_prompt.md, preserving a .bak copy of the previous value
    Set {
        name: String,
        /// Inline prompt text (omit when using -f)
        content: Option<String>,
        /// Read prompt text from a file
        #[arg(short = 'f', long = "file")]
        file: Option<String>,
    },
}
