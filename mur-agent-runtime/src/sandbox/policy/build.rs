use super::*;

impl SandboxPolicy {
    pub fn from_entitlements(ent: &Entitlements, agent_home: &Path) -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        let expand = expand_entitlement_path;
        // Every grant discarded below is recorded, not just warned about: a
        // WARN in a 30MB log is not an answer to "what can this agent write".
        let mut dropped: Vec<mur_common::agent::DroppedGrant> = Vec::new();

        // USER-DECLARED read/write entitlement paths are existence-checked at
        // profile-build time and dropped (fail-closed, warned) if missing.
        // Rationale (Issue 16): a `fs_write`/`fs_read` entry that names a
        // nonexistent path (e.g. a removed git worktree) still gets emitted
        // as an ordinary SBPL `subpath` grant with no existence requirement,
        // so `sandbox_init_with_parameters` accepts it silently — but the
        // resulting unresolvable grant was observed to destabilize *other*
        // unrelated `file-write*` checks under the same compiled policy
        // (30s tool-call hangs, not EPERM, until the agent restarts with the
        // dead path recreated or removed from entitlements). Mirrors the
        // fail-closed discipline `resolve_binary_path`/`is_executable_file`
        // already apply to `spawn_allowed_paths` below.
        let fs_read: Vec<PathBuf> = ent
            .filesystem
            .read
            .iter()
            .map(|s| expand(s))
            .filter(|p| {
                if std::fs::metadata(p).is_ok() {
                    true
                } else {
                    tracing::warn!(
                        path = %p.display(),
                        "filesystem read entitlement path does not exist on disk; \
                         agent will NOT have read access to it (dropping dead grant \
                         to avoid destabilizing the sandbox profile — Issue 16)"
                    );
                    dropped.push(mur_common::agent::DroppedGrant {
                        path: p.display().to_string(),
                        verb: "read".into(),
                        reason: "path does not exist on disk".into(),
                    });
                    false
                }
            })
            .collect();
        let fs_read = with_resolved_aliases(fs_read);
        // Drop any user-declared read grant that reaches the credential store
        // or a sibling's signing key, BEFORE anything else is added (#850).
        //
        // Only the user-declared set is partitioned: everything appended below
        // — the fleet_run carve-in, the runtime's own central-store reads, the
        // system paths — is chosen by this function and must never be dropped.
        // Partitioning the finished list instead would silently discard e.g.
        // `/private/tmp` whenever `mur_home` happens to nest under a system
        // read path, which is exactly what a test run surfaced.
        //
        // Landlock has no deny rule, so an overlapping grant cannot be carved
        // and is dropped whole, fail-closed. macOS reaches the same posture the
        // other way: it installs the grant and re-closes those paths with
        // `deny file-read*` clauses emitted after every allow (last-match-wins).
        let launch_chain = crate::sandbox::launch_chain::LaunchChain::new(agent_home);
        let (kept_reads, dropped_read_grants) =
            crate::sandbox::linux::partition_read_grants(&fs_read, &launch_chain);
        for p in &dropped_read_grants {
            tracing::warn!(
                path = %p.display(),
                "filesystem READ entitlement reaches the credential store or a \
                 sibling signing key; the whole grant is dropped (a protected \
                 path inside a grant cannot be carved out)"
            );
            dropped.push(mur_common::agent::DroppedGrant {
                path: p.display().to_string(),
                verb: "read".into(),
                reason: "reaches the credential store or a sibling signing key".into(),
            });
        }
        let mut fs_read = kept_reads;

        // Issue #004: a `git worktree` of a granted checkout lives OUTSIDE
        // that checkout, so no prefix grant can reach it and the user was
        // forced to re-grant the same repo under a second path.
        //
        // This must happen in the kernel policy too, not only in the
        // call-time tool gate: Landlock and SBPL are sealed once at startup
        // and cannot be asked a question later, so every worktree has to be
        // enumerated here. Aligning both layers is the point — a grant the
        // kernel allows but the tool gate refuses is the exact split that
        // sent an agent probing `/tmp` (see `for_file_tools`).
        //
        // Derived paths are appended to the POLICY only. They are never
        // written back to `profile.yaml`: an entitlement the user did not
        // type must not silently appear in the file they audit, and
        // recomputing each start means a pruned worktree loses access with no
        // stale grant left behind. `worktrees_of` already skips worktrees
        // missing from disk, keeping the Issue 16 no-dead-grants rule.
        //
        // `fs_deny` is deliberately NOT expanded this way — see
        // `tools::fs_policy::under_any_or_worktree`.
        for granted in fs_read.clone() {
            for wt in mur_common::worktree::worktrees_of(&granted) {
                if !fs_read.contains(&wt) {
                    tracing::debug!(
                        worktree = %wt.display(),
                        main = %granted.display(),
                        "granting read to a git worktree of a granted checkout (#004)"
                    );
                    fs_read.push(wt);
                }
            }
        }

        let fs_write: Vec<PathBuf> = ent
            .filesystem
            .write
            .iter()
            .map(|s| expand(s))
            .filter(|p| {
                if std::fs::metadata(p).is_ok() {
                    true
                } else {
                    tracing::warn!(
                        path = %p.display(),
                        "filesystem write entitlement path does not exist on disk; \
                         agent will NOT have write access to it (dropping dead grant \
                         to avoid destabilizing the sandbox profile — Issue 16)"
                    );
                    dropped.push(mur_common::agent::DroppedGrant {
                        path: p.display().to_string(),
                        verb: "write".into(),
                        reason: "path does not exist on disk".into(),
                    });
                    false
                }
            })
            .collect();
        let mut fs_write = with_resolved_aliases(fs_write);
        // fs_deny entries are kept verbatim even if the path doesn't exist:
        // dropping a dead deny entry would be fail-OPEN — if the path later
        // appears (mount, create, restore) the agent would silently regain
        // access we meant to permanently deny. Deny-side dead paths are
        // harmless (confirmed: no hang mechanism triggers off `fs_deny`).
        // A deny also gets its resolved alias, so a symlink cannot be used to
        // reach the target under a name the deny does not mention.
        let mut fs_deny: Vec<PathBuf> =
            with_resolved_aliases(ent.filesystem.deny.iter().map(|s| expand(s)).collect());

        // Self-protection (issue #712): unconditionally deny the agent's own
        // SELF_PROTECTED_AGENT_FILES, even when a write grant covers them
        // (e.g. a broad grant on the agents root, or agent_home itself which
        // is force-granted just below). Without this, an agent can rewrite
        // its own entitlements and self-restart to apply them — the seal is
        // generated FROM the profile, so profile self-write defeats it. On
        // macOS the SBPL builder emits fs_deny after the write allows
        // (last-match-wins) so this beats any overlapping grant; Landlock
        // cannot express deny-within-allow, so the same paths are also
        // injected into the tool-level gate (`tools::fs_policy::for_file_tools`).
        for f in SELF_PROTECTED_AGENT_FILES {
            let p = agent_home.join(f);
            if !fs_deny.contains(&p) {
                fs_deny.push(p);
            }
        }

        // agent_home is always read+write — runtime cannot function without it.
        // (Its existence is a precondition of the runtime starting at all —
        // profile.yaml must already live there — so no create_dir_all needed.)
        if !fs_write.contains(&agent_home.to_path_buf()) {
            fs_write.push(agent_home.to_path_buf());
        }

        // The shared channel store (`<mur_home>/channels`) is runtime-owned: a
        // delegated agent appends its OWN signed reply there (peer-writes-own,
        // v3d-2). Always grant write regardless of the user's fs entitlement,
        // else `channel/delegate` self-reply silently fails on agents whose write
        // allowlist omits ~/.mur/channels (agent_home is <mur_home>/agents/<name>).
        if let Some(channels) = agent_home
            .parent()
            .and_then(|p| p.parent())
            .map(|m| m.join("channels"))
            && !fs_write.contains(&channels)
        {
            // Ensure the directory exists before granting it — same idiom as
            // the VLC `runtime_dir` precedent in supervisor/seal.rs. Unlike
            // user-declared entries, this path is runtime-owned so we create
            // it rather than drop the grant (Issue 16: a dead grant here
            // would destabilize other file-write* checks under this policy).
            let _ = std::fs::create_dir_all(&channels);
            fs_write.push(channels);
        }

        // `<mur_home>/open-items.jsonl` is the `open_item` built-in's only
        // write target. That tool's doc comment claimed an agent needs no
        // filesystem grant to use it, but nothing ever put the file in this
        // allowlist, so "record a todo" failed for every sandboxed agent —
        // and the failure surfaced as a bare `open <path>` with the errno
        // dropped, so the agent could only guess at the cause. Same
        // create-before-grant idiom as `channels`: an append-only log starts
        // out absent, and Landlock skips rules on paths that don't exist at
        // seal time.
        if let Some(items) = agent_home
            .parent()
            .and_then(|p| p.parent())
            .map(|m| m.join(mur_open_items::LOG_FILE))
            && !fs_write.contains(&items)
        {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&items);
            fs_write.push(items);
        }

        // `<mur_home>/index` holds three things with very different trust
        // requirements: the channels read-model subdir (`index/channels/`,
        // channels.db + WAL/SHM), the `*.lance` retrieval stores, and
        // `capabilities.json` (which the daemon injects UNSIGNED into the
        // operator's Claude session on every SessionStart — a prompt-
        // injection surface). Only `index/channels/` is granted here: a
        // delegated agent's self-append refreshes channels.db's
        // `updated_at` row, and without this grant SQLite maps the denied
        // write to SQLITE_READONLY, so every peer-writes-own append reports
        // a false failure (G3, live fleet run 2026-07-09). The rest of
        // `index/` is deliberately excluded — sandboxed members must not be
        // able to write capabilities.json (it feeds that unsigned inject)
        // or the lance stores (they shape retrieval). Same create-before-
        // grant idiom as `channels` (Landlock skips rules on paths that
        // don't exist at seal time).
        if let Some(channel_index_dir) = agent_home
            .parent()
            .and_then(|p| p.parent())
            .map(|m| m.join("index").join("channels"))
            && !fs_write.contains(&channel_index_dir)
        {
            let _ = std::fs::create_dir_all(&channel_index_dir);
            fs_write.push(channel_index_dir);
        }

        // `<mur_home>/artifacts/<agent>` — where the system prompt's
        // output-locations rule (`task_runner::OUTPUT_LOCATIONS_RULE`) tells
        // every agent to put reports, quarantined files and scratch output.
        // Nothing granted it, so an agent following its own instructions was
        // refused; on 2026-09-13 one then reached for `/tmp`, and that denial
        // withdrew `bash` for the rest of the turn. MUR telling an agent to
        // write somewhere it forbids is the same contradiction `PATH_FORMS`
        // exists to prevent, one layer down.
        //
        // Scoped to THIS agent's subdir, never `artifacts/` itself: every
        // other agent's output lives there, and a prompt-injected agent must
        // not be able to rewrite a sibling's report. Same create-before-grant
        // idiom as `channels` (Landlock skips rules on paths absent at seal
        // time, and the rule names `<run>` subdirs the agent creates itself).
        if let (Some(mur_home), Some(agent_name)) = (
            agent_home.parent().and_then(|p| p.parent()),
            agent_home.file_name(),
        ) {
            let mine = mur_home.join("artifacts").join(agent_name);
            if !fs_write.contains(&mine) {
                let _ = std::fs::create_dir_all(&mine);
                fs_write.push(mine);
            }
        }

        // `<mur_home>/tmp/<agent>` — the per-agent scratch dir children get
        // as `TMPDIR`. Path comes from `agent_paths` so this grant and the
        // file-tool gate (`tools::fs_policy::for_file_tools`) cannot drift.
        // Created `0700` before granting (Landlock skips absent paths). On
        // helper Err this is the ONE place that logs; the tool gate skips
        // silently so one fault is not reported twice.
        let scratch_dir = match crate::agent_paths::agent_scratch_dir(agent_home) {
            Ok(scratch) => {
                if let Err(e) = crate::agent_paths::ensure_scratch_dir(&scratch) {
                    tracing::warn!(path = %scratch.display(), %e, "scratch dir not prepared");
                }
                if !fs_write.contains(&scratch) {
                    fs_write.push(scratch.clone());
                }
                Some(scratch)
            }
            Err(e) => {
                tracing::error!(
                    agent_home = %agent_home.display(),
                    %e,
                    "scratch dir not granted"
                );
                None
            }
        };

        // fleet_run carve-ins (config-gated, deny-by-default): when THIS agent
        // is allowlisted in `~/.mur/config.yaml` `fleet_run.agents`, the
        // spawned `mur fleet run` / `mur deep-research` child (which inherits
        // this sandbox) needs the fleet-runner state dirs writable and the
        // `mur` binary spawnable. Gate lives in the global config — writable
        // by no agent — so a prompt-injected agent cannot widen it. Same
        // create-before-grant idiom as `channels` above. Reads need no grant
        // here (macOS reads are allow-default; the narrow read set for
        // Landlock is added to fs_read below alongside system paths).
        let mut fleet_run_enabled = false;
        if let (Some(mur_home), Some(agent_name)) = (
            agent_home.parent().and_then(|p| p.parent()),
            agent_home.file_name().and_then(|n| n.to_str()),
        ) && crate::tools::fleet_run::agent_enabled(mur_home, agent_name)
        {
            fleet_run_enabled = true;
            for dir in fleet_run_write_dirs(mur_home) {
                if !fs_write.contains(&dir) {
                    let _ = std::fs::create_dir_all(&dir);
                    fs_write.push(dir);
                }
            }
            for p in [mur_home.join("models.yaml"), mur_home.join("cache")] {
                if std::fs::metadata(&p).is_ok() && !fs_read.contains(&p) {
                    fs_read.push(p);
                }
            }
        }

        // The runtime's OWN central-store reads. macOS is allow-default so this
        // is a no-op there; on Linux Landlock is deny-by-default and WITHOUT
        // these the runtime cannot read its own configuration —
        // `Config::load_or_default` silently returns defaults (so the agent
        // runs on the wrong model), the compress hook never engages, and
        // `channel/delegate` cannot read a fleet definition. The failure is
        // silent in every case, which is why it went unnoticed.
        //
        // This is not a widening of the trust boundary: these are reads the
        // runtime already performs unconditionally, enumerated in the audit
        // (docs/superpowers/specs/2026-08-18-agent-read-confinement-audit.md
        // §7.1). Existence-checked like every other grant (Issue 16).
        if let Some(mur_home) = agent_home.parent().and_then(|p| p.parent()) {
            for p in [
                mur_home.join("config.yaml"),
                mur_home.join("compress.yaml"),
                mur_home.join("compress"),
                mur_home.join("fleets"),
            ] {
                if std::fs::metadata(&p).is_ok() && !fs_read.contains(&p) {
                    fs_read.push(p);
                }
            }
        }

        // Peer PUBLIC key material (`identity.pub` + `rotations.jsonl`), so a
        // sandboxed process can verify signed channel events. macOS already
        // allows these (allow-default, and the launch chain denies only
        // `identity.key`); Landlock grants nothing it is not told about, so
        // without this every peer key is unresolvable and verification
        // degrades silently — `verify_event` cannot tell "no key" from "no
        // signature". Public by construction, so this widens nothing.
        // `agents/` WHOLE, not two files per agent (#850 option (c) step 3).
        // Now that private keys live under `keys/`, the agents tree holds only
        // public material — profiles, `identity.pub`, `rotations.jsonl`, run
        // state — so granting it entire is the same posture macOS has always
        // had under `(allow default)`, and it has no after-seal gap: an agent
        // created later is inside the subtree, where an enumerated pair of
        // files per agent could never have named it.
        if let Some(mur_home) = agent_home.parent().and_then(|p| p.parent()) {
            let agents = mur_home.join("agents");
            if std::fs::metadata(&agents).is_ok() && !fs_read.contains(&agents) {
                fs_read.push(agents);
            }
        }

        // Standard system read paths: libraries, certs, DNS config.
        let system_read = system_read_paths();
        for p in system_read {
            if !fs_read.contains(&p) {
                fs_read.push(p);
            }
        }

        // Standard binary exec paths (needed for MCP spawn + shell tools).
        let fs_exec = system_exec_paths(&home);

        // Search dirs for resolving bare `spawn.allowed` binary names
        // (Issue 17) — one source shared with the MCP child PATH, see
        // `sandbox::search_dirs`.
        let spawn_search_dirs = crate::sandbox::search_dirs::spawn_search_dirs(&home);

        // Resolve each allowlisted binary name to EVERY absolute,
        // canonicalized, executable path it matches (Issue 17): a bare name
        // may resolve to multiple literal paths (e.g. the same binary name
        // present under both Homebrew and the developer tools), and all of
        // them must be granted so the resolved path matches whichever one
        // actually executes at spawn time. An entry that names an absolute
        // path is canonicalized and kept only if executable — no directory
        // search. Entries that resolve to nothing are dropped with a
        // warning — fail-closed for that one binary, never fail-open for
        // the profile. Canonicalization failures are dropped the same way
        // (Issue 16 discipline: never emit an unresolvable grant).
        let spawn_mode = ent.processes.spawn.mode;
        let mut spawn_allowed_paths: Vec<PathBuf> = Vec::new();
        // fleet_run: the child is the `mur` binary itself. Grant the exact
        // path `fleet_run` will exec (`exec_dirs::mur_cli`) — NOT the bare
        // name. A bare name is resolved here by scanning the search dirs,
        // while the exec resolves it through PATH later, and the two answered
        // differently the moment a `brew` symlink landed after the seal
        // (2026-09-09: allowlisted `~/.local/bin/mur`, exec'd the Cellar copy,
        // EPERM). An absolute path takes the no-search branch below, so grant
        // and spawn cannot drift apart again.
        let mut spawn_names: Vec<String> = ent.processes.spawn.allowed.clone();
        if fleet_run_enabled {
            let cli = crate::exec_dirs::mur_cli().to_string_lossy().into_owned();
            if !spawn_names.contains(&cli) {
                spawn_names.push(cli);
            }
        }
        for name in &spawn_names {
            let mut matched_any = false;
            if name.contains(std::path::MAIN_SEPARATOR) {
                let candidate = Path::new(name);
                if is_executable_file(candidate)
                    && let Ok(canon) = std::fs::canonicalize(candidate)
                {
                    matched_any = true;
                    let differs = canon != candidate;
                    if !spawn_allowed_paths.contains(&canon) {
                        spawn_allowed_paths.push(canon);
                    }
                    // A relocated-home ancestor (e.g. a symlinked package
                    // dir) means the exec path Seatbelt actually checks at
                    // spawn time may be either the original or the
                    // canonical form — keep both (dedup above/below still
                    // applies to each individually).
                    if differs && !spawn_allowed_paths.contains(&candidate.to_path_buf()) {
                        spawn_allowed_paths.push(candidate.to_path_buf());
                    }
                }
            } else {
                for dir in &spawn_search_dirs {
                    let candidate = dir.join(name);
                    if !is_executable_file(&candidate) {
                        continue;
                    }
                    match std::fs::canonicalize(&candidate) {
                        Ok(canon) => {
                            matched_any = true;
                            let differs = canon != candidate;
                            if !spawn_allowed_paths.contains(&canon) {
                                spawn_allowed_paths.push(canon);
                            }
                            // See the relocated-home comment above: keep
                            // both forms when they differ.
                            if differs && !spawn_allowed_paths.contains(&candidate) {
                                spawn_allowed_paths.push(candidate.clone());
                            }
                        }
                        Err(err) => {
                            tracing::warn!(
                                binary = %name,
                                path = %candidate.display(),
                                error = %err,
                                "spawn allowlist candidate could not be canonicalized; \
                                 dropping this match"
                            );
                        }
                    }
                }
            }
            if !matched_any {
                tracing::warn!(
                    binary = %name,
                    "spawn allowlist entry could not be resolved to an executable; dropping"
                );
                // Record it, don't just warn: a dropped filesystem grant lands
                // in `running.lock`'s `dropped` while a dropped spawn grant
                // used to exist only as a WARN in a multi-megabyte log — 264
                // silent drops across the fleet before anyone looked (one
                // agent lost its browser tooling on every start for weeks).
                dropped.push(mur_common::agent::DroppedGrant {
                    path: name.clone(),
                    verb: "spawn".into(),
                    reason: "no executable of that name in the search dirs".into(),
                });
            }
        }

        // Strict-mode shell guarantee: the runtime, not the profile author,
        // is responsible for keeping the bash TOOL functional once the
        // system exec-path exemption is fenced off. Resolve the same
        // `bash` the bash tool itself spawns (see `tools/bash.rs` --
        // `Command::new("bash")`, a PATH lookup) by searching the same
        // `spawn_search_dirs` used for allowlist resolution above --
        // on macOS this lands on `/bin/bash` -- and push its canonicalized
        // path into `spawn_allowed_paths` automatically. Strict contract:
        // the bash tool can still launch its shell; nothing else is
        // implied -- every other system binary stays fenced.
        if spawn_mode == SpawnMode::Strict {
            for dir in &spawn_search_dirs {
                let candidate = dir.join("bash");
                if !is_executable_file(&candidate) {
                    continue;
                }
                if let Ok(canon) = std::fs::canonicalize(&candidate) {
                    if !spawn_allowed_paths.contains(&canon) {
                        spawn_allowed_paths.push(canon.clone());
                    }
                    if canon != candidate && !spawn_allowed_paths.contains(&candidate) {
                        spawn_allowed_paths.push(candidate.clone());
                    }
                    break;
                }
            }
        }

        // Derive the prefix grants from the resolved literals (Issue 17):
        // see the `spawn_allowed_prefixes` field doc for the parent/
        // grandparent-if-`bin` rule and the guard list.
        let mut spawn_allowed_prefixes: Vec<PathBuf> = Vec::new();
        for literal in &spawn_allowed_paths {
            let prefix = compute_spawn_prefix(literal, &home);
            if prefix.exists() && !spawn_allowed_prefixes.contains(&prefix) {
                spawn_allowed_prefixes.push(prefix);
            }
        }

        // Explicit build-lane grants (`processes.spawn.allowed_dirs`). A
        // toolchain that compiles its own executables cannot be expressed as
        // a list of binaries — a Rust build execs build scripts, proc-macro
        // shims and test binaries at hash-suffixed paths that do not exist
        // until the build creates them. Fail closed the same way the literal
        // list does: an entry that is missing, is not a directory, or cannot
        // be canonicalized is dropped rather than widened.
        for dir in &ent.processes.spawn.allowed_dirs {
            let expanded = expand(dir);
            let mut candidates = vec![expanded.clone()];
            candidates.extend(mur_common::worktree::worktrees_of(&expanded));
            for candidate in candidates {
                let Ok(canon) = std::fs::canonicalize(&candidate) else {
                    continue;
                };
                if !canon.is_dir() {
                    continue;
                }
                // A grant of `/`, `/usr`, the home directory, or a bare
                // `/Volumes/<mount>` is not a build lane; it is every binary
                // reachable under it. Same guard the derived prefixes use.
                if is_guarded_prefix(&canon, &home) {
                    continue;
                }
                if !spawn_allowed_prefixes.contains(&canon) {
                    spawn_allowed_prefixes.push(canon);
                }
            }
        }

        let (net_allow_ports, net_allow_hosts, net_loopback_allowed) =
            match ent.network.outbound.mode {
                NetworkOutboundMode::Unrestricted => (None, None, false),
                NetworkOutboundMode::Restricted => {
                    // Issue #006: the built-in web set plus whatever extra
                    // ports the user explicitly declared. Deduped so a
                    // redundant re-declaration of 443 cannot emit two SBPL
                    // clauses / two Landlock rules. Only this arm reads
                    // `allow_ports`: Off and ProxyOnly below ignore it, so a
                    // stale entry in a profile whose mode was later tightened
                    // cannot reopen general TCP.
                    let mut ports = RESTRICTED_GENERAL_PORTS.to_vec();
                    for p in &ent.network.outbound.allow_ports {
                        if !ports.contains(p) {
                            ports.push(*p);
                        }
                    }
                    let hosts = Some(ent.network.outbound.allow_hosts.clone());
                    (Some(ports), hosts, true)
                }
                NetworkOutboundMode::ProxyOnly => {
                    // Deny general TCP (empty-but-present list), keep the host
                    // allowlist so the runtime's own client can resolve its
                    // loopback LLM endpoint. Loopback carve-outs are added by the
                    // port-assembly helpers; net_loopback_allowed = true is what
                    // lets them fire despite the empty general list.
                    (
                        Some(vec![]),
                        Some(ent.network.outbound.allow_hosts.clone()),
                        true,
                    )
                }
                NetworkOutboundMode::Off => (Some(vec![]), Some(vec![]), false),
            };

        // Issue #004, write side. Mirrors the read-side derivation above: a
        // worktree of a granted checkout is enumerated into the sealed policy
        // so the kernel and the tool gate agree.
        //
        // Derived from the USER-DECLARED write grants only, not from the
        // finished list. Everything appended between there and here is
        // runtime-owned (`agent_home`, `channels/`, `index/channels/`,
        // `open-items.jsonl`) and is not a git checkout, so scanning it would
        // be pure cost — but more importantly, deriving off a list this
        // function built itself is how a widening rule quietly compounds.
        //
        // Placed BEFORE `partition_write_grants` on purpose: a derived grant
        // gets exactly the same launch-chain treatment as a typed one. A
        // worktree that somehow overlapped the launch chain must be dropped
        // fail-closed like any other grant, never smuggled in by being added
        // after the check.
        let declared_writes: Vec<PathBuf> =
            ent.filesystem.write.iter().map(|s| expand(s)).collect();
        for granted in declared_writes {
            for wt in mur_common::worktree::worktrees_of(&granted) {
                if !fs_write.contains(&wt) {
                    tracing::debug!(
                        worktree = %wt.display(),
                        main = %granted.display(),
                        "granting write to a git worktree of a granted checkout (#004)"
                    );
                    fs_write.push(wt);
                }
            }
        }

        // Linux Landlock cannot carve a protected path out of a grant (pure
        // allow-list), so any write grant overlapping the launch chain is
        // dropped whole here, fail-closed — and recorded for the runtime-doctor
        // (spec 2026-08-11). macOS carries the same field for symmetry; its
        // SBPL deny/re-allow carve-out needs no drop.
        let (_, dropped_grants) =
            crate::sandbox::linux::partition_write_grants(&fs_write, &launch_chain);
        // Only where the drop actually happens. Landlock cannot carve a grant,
        // so Linux drops it whole; macOS keeps the grant and re-closes the
        // overlap with `deny file-read*` clauses emitted after the allows
        // (last-match-wins), so the grant IS installed there. Recording these
        // on macOS would report a loss of access that did not occur.
        #[cfg(target_os = "linux")]
        for pth in &dropped_grants {
            dropped.push(mur_common::agent::DroppedGrant {
                path: pth.display().to_string(),
                verb: "write".into(),
                reason: "overlaps MUR's launch chain and cannot be carved (Landlock)".into(),
            });
        }
        // Export only what the kernel still grants: a later carve may have
        // pulled the path out of `fs_write`.
        let scratch_dir = scratch_dir.filter(|d| fs_write.contains(d));
        SandboxPolicy {
            dropped,
            fs_read,
            fs_write,
            scratch_dir,
            fs_deny,
            fs_exec,
            spawn_mode,
            spawn_allowed_paths,
            spawn_allowed_prefixes,
            net_allow_ports,
            net_allow_loopback_ports: Vec::new(),
            net_loopback_allowed,
            net_allow_hosts,
            memory_limit_mb: Some(ent.limits.memory_mb),
            launch_chain,
            dropped_grants,
            dropped_read_grants,
        }
    }
}
