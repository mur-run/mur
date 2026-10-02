use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct McpServerEntry {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,

    /// SHA-256 (hex, lowercase) of the binary at `command`'s resolved
    /// path, captured at install time. `None` means the entry was
    /// added before B0 M9.1 (back-compat) and rule-6 enforcement is
    /// not applied — the supervisor will warn but not block.
    /// (B0 rule 6 / M9.1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_sha256: Option<String>,

    /// SHA-256 (hex, lowercase) of the canonical-JSON of the MCP's
    /// `tools/list` response, captured at install time. `None` means
    /// the install path skipped the description probe (e.g. the MCP
    /// uses a non-stdio transport or the binary couldn't be reached)
    /// or the entry pre-dates M9. (B0 rule 6 / M9.1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_hash: Option<String>,

    /// Display-only publisher metadata captured at install time so
    /// the user can recall what they consented to. `None` for older
    /// entries. (B0 rule 6 / M9.1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<McpPublisherInfo>,

    /// RFC3339 timestamp of when the entry was added or last
    /// re-approved by the user via `mur agent mcp pin`. Used by the
    /// rug-pull dialog UX. `None` for older entries. (B0 rule 6 / M9.1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<chrono::DateTime<chrono::Utc>>,

    /// Per-tool-call timeout for this server, in seconds. `None` uses the
    /// runtime default. Slow tools (e.g. `video_analyze`: transcript fetch
    /// + local-model map-reduce) need a longer budget than the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u32>,

    /// Per-server outbound egress override. `None` = inherit the agent-level
    /// policy (default; unchanged behavior). `Restricted` routes this server's
    /// child through the runtime egress proxy with `allow_hosts` (advisory).
    /// See `docs/superpowers/plans/2026-06-26-mcp-per-server-egress.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<McpServerNetwork>,

    /// HTTP(S) base URL for a remote (Streamable-HTTP or SSE) MCP server.
    /// Mutually exclusive with `command` in practice; `None` = stdio transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Authentication credentials for a remote MCP server.
    /// `None` = no auth (or stdio transport).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<McpAuth>,

    /// External programs this artifact needs at runtime (portable-deps spec).
    /// Absent → empty; resolved by `mur agent/fleet doctor` + `install-deps`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_programs: Vec<ProgramDep>,

    /// Paths this server writes state into at runtime, declared at install
    /// time so the sandbox can be told about them (issue #1161).
    ///
    /// Distinct from everything above: `command`, `args` and `package`
    /// describe how the server is *launched*, and #1158 already syncs what the
    /// rewritten launch line needs. These are what the server touches once it
    /// is running — a property of the server, not of the command MUR rewrote.
    /// `@wonderwhy-er/desktop-commander` wants three of them under `$HOME` and
    /// exits 1 before answering `initialize` without them.
    ///
    /// Granted read+write, and **created if missing** at install time. The
    /// sandbox drops entitlement paths that do not exist when the profile is
    /// sealed, so granting a directory the server has not created yet would be
    /// accepted and still denied by the kernel — see `reject_dead_grant`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state_paths: Vec<String>,

    /// Vendored package this entry launches, when MUR installed it itself.
    ///
    /// Present only for entries moved off a package runner by
    /// `mur agent mcp vendor`. Its existence is what makes the contents of an
    /// interpreter-launched server verifiable at all: `npx @scope/pkg` resolves
    /// on every spawn and pins nothing, whereas a vendored install lives in a
    /// directory MUR owns and can be checked before the agent comes up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<McpPackagePin>,
}

/// A package MUR installed itself, and the fingerprint that proves the
/// installed tree hasn't changed.
///
/// `lockfile_sha256` hashes the install's `package-lock.json`, which already
/// records an integrity hash for every package in the dependency tree — so one
/// small file covers the whole tree, and startup verification stays cheap no
/// matter how large `node_modules` grows.
///
/// The lockfile pins what was *installed*. Editing a file inside
/// `node_modules` afterwards would not change it; catching that needs a full
/// tree hash, which is deliberately not done here — see the module docs on
/// `mur-core::cmd::agent_mcp_vendor` for where that line is drawn.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct McpPackagePin {
    /// Package ecosystem — `npm` today.
    pub runner: String,
    /// Package name, including any `@scope/` prefix.
    pub name: String,
    /// Exact installed version.
    pub version: String,
    /// Directory MUR installed into, absolute.
    pub install_dir: String,
    /// SHA-256 (lowercase hex) of `<install_dir>/package-lock.json`.
    pub lockfile_sha256: String,

    /// How many packages in the installed tree published no registry
    /// signature, as reported by `npm audit signatures` at vendor time.
    ///
    /// `None` — the audit did not run (npm too old, or offline).
    /// `Some(0)` — every package in the tree carried a verified signature.
    /// `Some(n)` — `n` packages are unsigned; the rest verified.
    ///
    /// A signature that verifies proves the bytes came from the registry, which
    /// the content hash cannot: it would faithfully pin a poisoned cache. An
    /// *invalid* signature is not recorded here because it blocks the vendor
    /// outright — that is an integrity failure, not a property to note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signatures_missing: Option<u32>,

    /// SLSA predicate type of the package's build provenance, when it
    /// publishes one — e.g. `https://slsa.dev/provenance/v1`. `None` means no
    /// attestation was published (still the common case).
    ///
    /// Provenance ties a release back to a source repository and CI run, and
    /// is the only signal here that can catch a **malicious publish**: a
    /// content hash pins whatever was released, faithfully preserving a
    /// poisoned version rather than detecting it. Recorded and shown, never
    /// required — ecosystem coverage is far too thin to gate on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
}

impl McpPackagePin {
    /// Name of the lockfile whose hash is `lockfile_sha256`.
    ///
    /// npm writes `package-lock.json` itself; for PyPI, MUR generates one with
    /// `uv pip compile --generate-hashes`, which records a sha256 for every
    /// package in the resolved tree — the same property that lets one small
    /// file stand in for the whole install.
    pub fn lockfile_name(&self) -> &'static str {
        match self.runner.as_str() {
            "pypi" => "requirements.lock",
            _ => "package-lock.json",
        }
    }

    /// Absolute path of the lockfile this pin covers.
    ///
    /// The startup check, `inspect`, and the deep audit all resolve it through
    /// here, so a newly supported ecosystem cannot end up verified against the
    /// wrong file in one of them and silently pass.
    pub fn lockfile_path(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.install_dir).join(self.lockfile_name())
    }
}

/// Authentication scheme for a remote (HTTP) MCP server.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum McpAuth {
    /// Static bearer token stored as a secret reference.
    Bearer { token: crate::secret::SecretRef },
    /// OAuth 2.1 token, with dynamic client registration state.
    Oauth(OauthAuth),
}

/// OAuth 2.1 state persisted alongside remote MCP entry.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct OauthAuth {
    /// Authorization-server token endpoint (from discovery).
    pub token_endpoint: String,
    /// Client id from dynamic client registration.
    pub client_id: String,
    /// Keychain ref to access token.
    pub access_token: crate::secret::SecretRef,
    /// Keychain ref refresh token, if server issued one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<crate::secret::SecretRef>,
    /// Unix-epoch seconds access token expires (0 = unknown).
    #[serde(default)]
    pub expires_at: u64,
}

/// How an MCP server's outbound network is scoped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpNetMode {
    /// No per-server policy and no proxy — the default.
    ///
    /// NOT "inherits `entitlements.network.outbound.allow_hosts`", despite the
    /// name. That list is enforced in-process (a DNS guard on the runtime's own
    /// HTTP client, plus the B0 gate on the agent's `network.*` tools), and a
    /// spawned server never runs either. What a server here actually inherits
    /// is the OS sandbox — which restricts by PORT, with the host left open.
    ///
    /// So an agent whose `allow_hosts` names one API still lets an `Inherit`
    /// server reach any host on an allowed port. Use `Restricted` to bound a
    /// server by host. The variant keeps its name because it is a serialized
    /// wire value; the lie was the doc, and it is fixed here rather than
    /// migrated.
    #[default]
    Inherit,
    /// Allow only `allow_hosts`, routed through the runtime egress proxy.
    Restricted,
    /// Allow ALL hosts EXCEPT `deny_hosts`, routed through the runtime egress
    /// proxy, with every CONNECT audited. For trusted-but-broad tools (e.g. a
    /// web-research browser) that cannot enumerate their destinations. Requires
    /// explicit operator consent (records `authorization`); downgraded to
    /// `Inherit` on import (lowest trust). Advisory enforcement (see egress_proxy).
    BroadAudited,
    /// No outbound for this server at all.
    Off,
}

/// Env var name a sandboxed MCP child reads to self-enforce the operator's
/// `deny_hosts` overlay on connections the egress proxy cannot observe (e.g.
/// `mur-research-gateway`'s tier-2/3 browser subprocesses — the proxy only
/// sees tier-1 `reqwest` traffic). `mur-agent-runtime`'s `proxy_env_for` sets
/// this on the child's env alongside the proxy vars; a cooperating child
/// (currently `mur-research-gateway`, via `config::load`) reads it to source
/// its own deny list. Single definition shared by both crates (CLAUDE.md
/// rule 1: no duplicated literal).
pub const ENV_MCP_DENY_HOSTS: &str = "MUR_RESEARCH_DENY_HOSTS";

/// Per-MCP-server outbound egress policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct McpServerNetwork {
    #[serde(default)]
    pub mode: McpNetMode,
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    /// Deny overlay for `BroadAudited` mode: hosts blocked even though all
    /// others are allowed. Ignored by `Restricted`/`Inherit`/`Off`.
    #[serde(default)]
    pub deny_hosts: Vec<String>,
    /// Who authorized a `BroadAudited` grant, and when. `None` for other modes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization: Option<EgressAuthorization>,
}

/// A plugin-group imported by one agent (add-on Phase 2). Self-contained:
/// members are installed PER-AGENT (skills under
/// `~/.mur/agents/<a>/skills/`, mcp appended to this profile's
/// `mcp_servers`). No global library, no refcounting.
///
/// Fail-closed: `enabled` defaults to `false`. Only an explicit user
/// toggle (CLI/Hub) or a trusted native installer flips it true — the
/// importer always constructs it `false`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AddonRef {
    /// e.g. "superpowers" (local) or "superpowers@claude-plugins-official".
    pub id: String,
    /// Provenance, free-text. e.g. "claude-local:superpowers@6.0.3".
    pub source: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    /// Content-hash pin over the imported skill/command manifests, recorded
    /// at import. `None` on legacy refs. Enables drift detection + refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// The re-fetchable source (the original `import` argument: a local path
    /// or `owner/repo`), distinct from the free-text provenance `source`.
    /// `None` on legacy refs. Used by `reimport`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_ref: Option<String>,
    /// The `--plugin <name>` selector used at import time to pick one plugin
    /// out of a multi-plugin marketplace `fetch_ref`. `None` when the source
    /// was a single-plugin dir/repo, or on legacy refs. Used by `reimport` so
    /// a marketplace add-on can be re-fetched without re-specifying it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_plugin: Option<String>,
}

/// Display-only publisher metadata captured at install time. None of
/// the fields are validated against any external authority — they're
/// shown to the user during the install confirm prompt and reproduced
/// in `mur agent mcp inspect` output so the user can audit who they
/// thought they were trusting. (B0 rule 6 / M9.1)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct McpPublisherInfo {
    /// Free-form publisher identifier — e.g. `"Anthropic"`,
    /// `"@github-user-alice"`, or whatever `serverInfo.name` returned.
    pub name: String,

    /// Optional homepage / docs URL. Best-effort: extracted from the
    /// MCP's `serverInfo.metadata.homepage` or registry entry when
    /// available; otherwise left unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,

    /// Optional registry coordinate — e.g. `"@anthropic-mcp/weather@1.2.3"`.
    /// Used purely for display; not consumed by any verification path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_id: Option<String>,
}

#[cfg(test)]
mod mcp_pin_tests {
    use super::*;

    /// Pre-M9 profiles must continue to deserialize with the new
    /// optional fields absent. Round-trip: serialize back out and
    /// confirm the optional fields don't leak into the YAML.
    #[test]
    fn pre_m9_entry_roundtrips_without_pin_fields() {
        let yaml = r#"
name: weather
command: /opt/mcp/weather
args: ["--port", "0"]
"#;
        let entry: McpServerEntry = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(entry.name, "weather");
        assert_eq!(entry.binary_sha256, None);
        assert_eq!(entry.description_hash, None);
        assert_eq!(entry.publisher, None);
        assert_eq!(entry.installed_at, None);

        // skip_serializing_if = "Option::is_none" must keep the YAML
        // free of empty pin fields when the entry is pre-M9.
        let out = serde_yaml_ng::to_string(&entry).unwrap();
        assert!(!out.contains("binary_sha256"), "got {out}");
        assert!(!out.contains("description_hash"), "got {out}");
        assert!(!out.contains("publisher"), "got {out}");
        assert!(!out.contains("installed_at"), "got {out}");
    }

    /// Full M9 entry with all fields set round-trips losslessly.
    #[test]
    fn full_m9_entry_roundtrips_all_fields() {
        let yaml = r#"
name: weather
command: /opt/mcp/weather
args: []
binary_sha256: "3f4abca8b0e6e2c1d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b81c"
description_hash: "9a01b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9c7e2"
publisher:
  name: "@anthropic-mcp/weather"
  homepage: "https://github.com/anthropic-mcp/weather"
  registry_id: "@anthropic-mcp/weather@1.2.3"
installed_at: "2026-05-06T08:00:00Z"
"#;
        let entry: McpServerEntry = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(
            entry
                .binary_sha256
                .as_deref()
                .unwrap()
                .starts_with("3f4abca8")
        );
        assert!(
            entry
                .description_hash
                .as_deref()
                .unwrap()
                .starts_with("9a01b2c3")
        );
        let pub_info = entry.publisher.clone().unwrap();
        assert_eq!(pub_info.name, "@anthropic-mcp/weather");
        assert_eq!(
            pub_info.homepage.as_deref(),
            Some("https://github.com/anthropic-mcp/weather"),
        );
        assert_eq!(
            pub_info.registry_id.as_deref(),
            Some("@anthropic-mcp/weather@1.2.3"),
        );
        let installed = entry.installed_at.unwrap();
        assert_eq!(installed.to_rfc3339(), "2026-05-06T08:00:00+00:00");
    }

    /// Partial — only the binary hash is set (e.g. probe failed but
    /// install proceeded). The supervisor still needs to be able to
    /// deserialize this without panicking.
    #[test]
    fn partial_pin_only_binary_sha_roundtrips() {
        let yaml = r#"
name: weather
command: /opt/mcp/weather
args: []
binary_sha256: "deadbeef00112233445566778899aabbccddeeff00112233445566778899aabb"
"#;
        let entry: McpServerEntry = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(
            entry.binary_sha256.as_deref(),
            Some("deadbeef00112233445566778899aabbccddeeff00112233445566778899aabb"),
        );
        assert_eq!(entry.description_hash, None);
        assert_eq!(entry.publisher, None);
    }

    /// Publisher with only the required `name` field — homepage and
    /// registry_id are optional.
    #[test]
    fn publisher_minimal_just_name() {
        let yaml = r#"
name: weather
command: /opt/mcp/weather
args: []
publisher:
  name: "alice"
"#;
        let entry: McpServerEntry = serde_yaml_ng::from_str(yaml).unwrap();
        let p = entry.publisher.as_ref().unwrap();
        assert_eq!(p.name, "alice");
        assert_eq!(p.homepage, None);
        assert_eq!(p.registry_id, None);

        // skip_serializing_if must omit the optional sub-fields too.
        let out = serde_yaml_ng::to_string(&entry).unwrap();
        assert!(!out.contains("homepage:"), "got {out}");
        assert!(!out.contains("registry_id:"), "got {out}");
    }
}

#[cfg(test)]
mod remote_mcp_tests {
    use super::*;

    #[test]
    fn mcp_entry_roundtrips_remote_bearer() {
        let e = McpServerEntry {
            name: "gh".into(),
            command: String::new(),
            url: Some("https://api.example.com/mcp".into()),
            auth: Some(McpAuth::Bearer {
                token: crate::secret::SecretRef::Env("GH_TOKEN".into()),
            }),
            ..Default::default()
        };
        let y = serde_yaml_ng::to_string(&e).unwrap();
        let back: McpServerEntry = serde_yaml_ng::from_str(&y).unwrap();
        assert_eq!(back.url.as_deref(), Some("https://api.example.com/mcp"));
        assert!(matches!(
            back.auth,
            Some(McpAuth::Bearer { ref token }) if *token == crate::secret::SecretRef::Env("GH_TOKEN".into())
        ));
        // A legacy stdio entry (no url/auth) still parses.
        let legacy: McpServerEntry =
            serde_yaml_ng::from_str("name: fs\ncommand: npx\nargs: [\"-y\",\"fs\"]\n").unwrap();
        assert!(legacy.url.is_none());
        assert!(legacy.auth.is_none());
    }
}

#[cfg(test)]
mod requires_programs_tests {
    #[test]
    fn mcp_entry_parses_requires_programs_and_defaults_empty() {
        let with = r#"
name: research-gateway
command: mur-research-gateway
requires_programs:
  - name: lightpanda
    detect: { file: "~/.mur/aura/lightpanda" }
    reason: "render tier"
    registry: lightpanda
"#;
        let e: crate::agent::McpServerEntry = serde_yaml::from_str(with).unwrap();
        assert_eq!(e.requires_programs.len(), 1);
        assert_eq!(e.requires_programs[0].name, "lightpanda");

        // Absent block → empty (back-compat).
        let without = "name: x\ncommand: y\n";
        let e2: crate::agent::McpServerEntry = serde_yaml::from_str(without).unwrap();
        assert!(e2.requires_programs.is_empty());
    }
}
