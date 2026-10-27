//! Spawn-allowlist resolution for [`SandboxPolicy::from_entitlements`]:
//! the literal executables and directory-prefix grants the kernel policy
//! installs for `processes.spawn`. Moved out of `build.rs` unchanged (#800-line rule).

use super::*;

/// Resolve `processes.spawn` into `(spawn_allowed_paths, spawn_allowed_prefixes)`.
/// Every entry that cannot be resolved is recorded in `dropped`.
pub(super) fn resolve_spawn_grants(
    ent: &Entitlements,
    home: &Path,
    fleet_run_enabled: bool,
    dropped: &mut Vec<mur_common::agent::DroppedGrant>,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let expand = expand_entitlement_path;
    // Search dirs for resolving bare `spawn.allowed` binary names
    // (Issue 17) — one source shared with the MCP child PATH, see
    // `sandbox::search_dirs`.
    let spawn_search_dirs = crate::sandbox::search_dirs::spawn_search_dirs(home);

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
        let prefix = compute_spawn_prefix(literal, home);
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
    // be canonicalized is dropped rather than widened. The drop is also
    // recorded (#2030): it lasts for the life of the process, and a
    // missing directory that is created later (the normal fleet order:
    // start members, then create worktrees and target dirs) otherwise
    // surfaces only as a bare `Operation not permitted` from the build.
    for dir in &ent.processes.spawn.allowed_dirs {
        let expanded = expand(dir);
        let mut candidates = vec![expanded.clone()];
        // The leaf form: `<worktree>/target` is usually created by the
        // first build, after the member started (#2058 follow-up).
        candidates.extend(mur_common::worktree::worktree_leaves_of(&expanded));
        for candidate in candidates {
            match resolve_build_lane_dir(&candidate, home) {
                Ok(BuildLaneDir::Live(canon)) => {
                    if !spawn_allowed_prefixes.contains(&canon) {
                        spawn_allowed_prefixes.push(canon);
                    }
                }
                // Not there yet, but its parent is: the grant is
                // installed against the canonical parent plus the literal
                // leaf, so the directory works once something creates it
                // (#2058). Not recorded as dropped: nothing was lost.
                Ok(BuildLaneDir::Pending(joined)) => {
                    tracing::info!(
                        path = %joined.display(),
                        "build-lane grant (processes.spawn.allowed_dirs) covers a \
                         directory that does not exist yet"
                    );
                    if !spawn_allowed_prefixes.contains(&joined) {
                        spawn_allowed_prefixes.push(joined);
                    }
                }
                Err(reason) => drop_build_lane_grant(dropped, &candidate, reason),
            }
        }
    }

    (spawn_allowed_paths, spawn_allowed_prefixes)
}
