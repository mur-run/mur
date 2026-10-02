use super::*;

/// Dev-discipline builtin names (spec 2026-07-23). Installation never
/// overwrites a same-named skill the user authored themselves.
pub(super) const NEW_DEV_SKILL_NAMES: &[&str] = &[
    "mur-dev",
    "mur-grilling",
    "mur-brainstorm",
    "mur-domain-modeling",
    "mur-writing-plans",
    "mur-tickets",
    "mur-executing-plans",
    "mur-delegate-dev",
    "mur-worktree",
    "mur-tdd",
    "mur-debugging",
    "mur-code-review",
    "mur-receiving-review",
    "mur-verification",
    "mur-finishing-branch",
    "mur-merge-conflicts",
    "mur-skill-authoring",
];

/// Publishers whose on-disk copies we own and may update in place. Shared with
/// the lifecycle sweep, which asks the same question about deletion.
use mur_common::skill::types::MUR_OWNED_PUBLISHERS as MUR_OFFICIAL_PUBLISHERS;

/// Never-shadow (spec 2026-07-23 §6): true when `name` is a dev-discipline
/// builtin AND `dir/skill.yaml` exists but was not published by MUR.
/// ponytail: publisher-based check; origin_hash edit detection if users
/// report clobbered local edits.
pub(super) fn dev_skill_shadowed_by_user(dir: &std::path::Path, name: &str) -> bool {
    if !NEW_DEV_SKILL_NAMES.contains(&name) {
        return false;
    }
    let Ok(existing) = std::fs::read_to_string(dir.join("skill.yaml")) else {
        return false;
    };
    match mur_common::skill::parse_canonical(&existing) {
        Ok(m) => !MUR_OFFICIAL_PUBLISHERS.contains(&m.publisher.as_str()),
        Err(_) => true,
    }
}

/// Install/update the MUR skill for AI tools that support skills.
/// Writes canonical copies to ~/.mur/skills/ and symlinks from tool dirs.
/// Returns true if any skill was written.
pub(crate) fn ensure_mur_skill(home: &std::path::Path, mur_root: &std::path::Path) -> Result<bool> {
    // `mur-browser/SKILL.md` (repo root, versioned like source — see
    // docs/superpowers/specs/2026-09-25-browser-skill-hub-design.md D1/D2) is
    // authored as markdown with Anthropic frontmatter, not canonical YAML like
    // every other entry below. `skill.yaml` is only ever read through
    // `parse_canonical` (mur_common::skill::loader), which rejects frontmatter
    // outright, so it has to be converted once here before it can join the
    // table that writes straight to `skill.yaml`.
    let browser_manifest =
        mur_common::skill::parse_markdown(include_str!("../../../../mur-browser/SKILL.md"))
            .context("parse mur-browser/SKILL.md")?;
    let browser_yaml = mur_common::skill::serialize_canonical(&browser_manifest)
        .context("serialize mur-browser manifest to canonical YAML")?;

    let skills: &[(&str, &str)] = &[
        ("browser", browser_yaml.as_str()),
        ("mur-context", include_str!("../../skills/mur_context.yaml")),
        ("mur-in", include_str!("../../skills/mur_in.yaml")),
        ("mur-out", include_str!("../../skills/mur_out.yaml")),
        ("mur-run", include_str!("../../skills/mur_run.yaml")),
        (
            "mur-native-tools",
            include_str!("../../skills/mur_native_tools.yaml"),
        ),
        (
            "mur-agent-manage",
            include_str!("../../skills/mur_agent_manage.yaml"),
        ),
        (
            "mur-project-index",
            include_str!("../../skills/mur_project_index.yaml"),
        ),
        (
            "mur-project-remove",
            include_str!("../../skills/mur_project_remove.yaml"),
        ),
        (
            "mur-project-search",
            include_str!("../../skills/mur_project_search.yaml"),
        ),
        ("mur-search", include_str!("../../skills/mur_search.yaml")),
        (
            "mur-compress",
            include_str!("../../skills/mur_compress.yaml"),
        ),
        (
            "mur-settlement",
            include_str!("../../skills/mur_settlement.yaml"),
        ),
        (
            "mur-session-remove",
            include_str!("../../skills/mur_session_remove.yaml"),
        ),
        ("vlc-control", include_str!("../../skills/vlc_control.yaml")),
        (
            "scene-explain",
            include_str!("../../skills/scene_explain.yaml"),
        ),
        (
            "video-analyze",
            include_str!("../../skills/video_analyze.yaml"),
        ),
        (
            "watch-together",
            include_str!("../../skills/watch_together.yaml"),
        ),
        (
            "parallel-code",
            include_str!("../../skills/parallel_code.yaml"),
        ),
        (
            "parallel-decompose",
            include_str!("../../skills/parallel_decompose.yaml"),
        ),
        (
            "mur-fleet-manage",
            include_str!("../../skills/mur_fleet_manage.yaml"),
        ),
        (
            "mur-fleet-loop",
            include_str!("../../skills/mur_fleet_loop.yaml"),
        ),
        (
            "mur-fleet-share",
            include_str!("../../skills/mur_fleet_share.yaml"),
        ),
        (
            "mur-workflow-author",
            include_str!("../../skills/mur_workflow_author.yaml"),
        ),
        (
            "mur-workflow-hitl",
            include_str!("../../skills/mur_workflow_hitl.yaml"),
        ),
        (
            "mur-workflow-delegate",
            include_str!("../../skills/mur_workflow_delegate.yaml"),
        ),
        (
            "mur-deep-research",
            include_str!("../../skills/mur_deep_research.yaml"),
        ),
        (
            "mur-capability",
            include_str!("../../skills/mur_capability.yaml"),
        ),
        (
            "mur-official",
            include_str!("../../skills/mur_official.yaml"),
        ),
        ("mur-notes", include_str!("../../skills/mur_notes.yaml")),
        ("mur-chat", include_str!("../../skills/mur_chat.yaml")),
        (
            "mur-agent-setup",
            include_str!("../../skills/mur_agent_setup.yaml"),
        ),
        (
            "mur-agent-mcp-wire",
            include_str!("../../skills/mur_agent_mcp_wire.yaml"),
        ),
        (
            "mur-agent-schedule",
            include_str!("../../skills/mur_agent_schedule.yaml"),
        ),
        (
            "mur-parallel-exec",
            include_str!("../../skills/mur_parallel_exec.yaml"),
        ),
        (
            "mur-parallel-tracks",
            include_str!("../../skills/mur_parallel_tracks.yaml"),
        ),
        (
            "mur-parallel-merge",
            include_str!("../../skills/mur_parallel_merge.yaml"),
        ),
        (
            "parallel-topology-guide",
            include_str!("../../skills/parallel_topology_guide.yaml"),
        ),
        (
            "deep-research-router",
            include_str!("../../skills/deep_research_router.yaml"),
        ),
        (
            "deep-research-worker",
            include_str!("../../skills/deep_research_worker.yaml"),
        ),
        (
            "deep-research-verify",
            include_str!("../../skills/deep_research_verify.yaml"),
        ),
        ("mur-dev", include_str!("../../skills/mur_dev.yaml")),
        (
            "mur-grilling",
            include_str!("../../skills/mur_grilling.yaml"),
        ),
        (
            "mur-brainstorm",
            include_str!("../../skills/mur_brainstorm.yaml"),
        ),
        (
            "mur-domain-modeling",
            include_str!("../../skills/mur_domain_modeling.yaml"),
        ),
        (
            "mur-writing-plans",
            include_str!("../../skills/mur_writing_plans.yaml"),
        ),
        ("mur-tickets", include_str!("../../skills/mur_tickets.yaml")),
        (
            "mur-executing-plans",
            include_str!("../../skills/mur_executing_plans.yaml"),
        ),
        (
            "mur-delegate-dev",
            include_str!("../../skills/mur_delegate_dev.yaml"),
        ),
        (
            "mur-worktree",
            include_str!("../../skills/mur_worktree.yaml"),
        ),
        ("mur-tdd", include_str!("../../skills/mur_tdd.yaml")),
        (
            "mur-debugging",
            include_str!("../../skills/mur_debugging.yaml"),
        ),
        (
            "mur-code-review",
            include_str!("../../skills/mur_code_review.yaml"),
        ),
        (
            "mur-receiving-review",
            include_str!("../../skills/mur_receiving_review.yaml"),
        ),
        (
            "mur-verification",
            include_str!("../../skills/mur_verification.yaml"),
        ),
        (
            "mur-finishing-branch",
            include_str!("../../skills/mur_finishing_branch.yaml"),
        ),
        (
            "mur-merge-conflicts",
            include_str!("../../skills/mur_merge_conflicts.yaml"),
        ),
        (
            "mur-skill-authoring",
            include_str!("../../skills/mur_skill_authoring.yaml"),
        ),
    ];

    let mur_skills_dir = mur_root.join("skills");

    // Clean up deprecated/renamed skills
    let deprecated_skills = ["mur-workflow", "mur"];
    let tool_dirs: &[&str] = &[".claude", ".augment", ".agents"];
    for old_name in &deprecated_skills {
        let old_canonical = mur_skills_dir.join(old_name);
        if old_canonical.exists() {
            let _ = std::fs::remove_dir_all(&old_canonical);
        }
        for tool_dir_name in tool_dirs {
            let old_link = home.join(tool_dir_name).join("skills").join(old_name);
            if old_link.exists() || old_link.symlink_metadata().is_ok() {
                let _ = std::fs::remove_file(&old_link);
                let _ = std::fs::remove_dir_all(&old_link);
            }
        }
    }

    // Write canonical YAML to ~/.mur/skills/<name>/skill.yaml
    // and render markdown to ~/.mur/skills/<name>/SKILL.md for AI tool compat.
    let mut shadowed: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (name, content) in skills {
        let dir = mur_skills_dir.join(name);
        if dev_skill_shadowed_by_user(&dir, name) {
            tracing::info!(
                skill = name,
                "skipping builtin install: user-authored skill of the same name exists (never-shadow)"
            );
            shadowed.insert(name);
            continue;
        }
        std::fs::create_dir_all(&dir)?;
        // Canonical YAML — consumed by M2 SkillLoader
        std::fs::write(dir.join("skill.yaml"), content)?;
        // Markdown rendering — consumed by existing AI tool hooks
        let md =
            mur_common::skill::yaml_to_markdown(content).unwrap_or_else(|_| content.to_string());
        std::fs::write(dir.join("SKILL.md"), md)?;
    }

    // Bundle assets (D3): skills whose SKILL.md references files under
    // `references/` need those files shipped alongside the manifest, not
    // just the manifest itself — `ensure_mur_skill` never had this capability
    // before `browser`. Runs after the manifest loop and before
    // `symlink_skill_dir` below, so the existing whole-directory symlink into
    // each tool dir (`.claude`/`.augment`/`.agents`) picks up `references/`
    // for free, with no change to that step.
    let bundle_assets: &[(&str, &str, &str)] = &[
        (
            "browser",
            "auth.md",
            include_str!("../../../../mur-browser/references/auth.md"),
        ),
        (
            "browser",
            "testing.md",
            include_str!("../../../../mur-browser/references/testing.md"),
        ),
        (
            "browser",
            "automation.md",
            include_str!("../../../../mur-browser/references/automation.md"),
        ),
    ];
    for (skill_name, file_name, content) in bundle_assets {
        if shadowed.contains(skill_name) {
            continue;
        }
        let refs_dir = mur_skills_dir.join(skill_name).join("references");
        std::fs::create_dir_all(&refs_dir)?;
        std::fs::write(refs_dir.join(file_name), content)?;
    }

    // Tool dirs to symlink into
    let tool_dirs: &[&str] = &[".claude", ".augment", ".agents"];

    for tool_dir_name in tool_dirs {
        let tool_base = home.join(tool_dir_name);
        if !tool_base.exists() && *tool_dir_name != ".agents" {
            continue;
        }
        let tool_skills = tool_base.join("skills");
        std::fs::create_dir_all(&tool_skills)?;

        for (name, _) in skills {
            if shadowed.contains(name) {
                continue;
            }
            let canonical = mur_skills_dir.join(name);
            let link = tool_skills.join(name);
            symlink_skill_dir(&canonical, &link)?;
        }
    }

    Ok(true)
}

/// Create a symlink from `link` -> `target`. If `link` exists as a regular
/// directory, remove it first. If it's already a correct symlink, skip.
pub(super) fn symlink_skill_dir(target: &std::path::Path, link: &std::path::Path) -> Result<()> {
    if link.exists() || link.symlink_metadata().is_ok() {
        // Check if it's already a correct symlink
        if let Ok(existing) = std::fs::read_link(link)
            && existing == target
        {
            return Ok(());
        }
        // Remove old dir or wrong symlink
        if link.is_dir()
            && !link
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
        {
            std::fs::remove_dir_all(link)?;
        } else {
            std::fs::remove_file(link)?;
        }
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)?;
    }

    #[cfg(not(unix))]
    {
        // Fallback: copy the directory contents. Must recurse — `browser`
        // (D3) was the first skill to carry a subdirectory (`references/`)
        // here, and `std::fs::copy` only copies files: handed a directory
        // entry it fails with "Access is denied" (os error 5) on Windows,
        // which is exactly what shipped in #1509's CI before this fix.
        copy_dir_recursive(target, link)?;
    }

    Ok(())
}

/// Recursive directory copy for platforms without symlinks (Windows). Not a
/// generic utility: mirrors exactly what a symlink would expose — every file
/// and subdirectory under `src`, nothing filtered.
///
/// Compiled on every platform (not `cfg(not(unix))`-gated) so its own test
/// runs in the macOS/Linux CI legs too, not only on the Windows leg that is
/// its only real caller — that asymmetry is exactly how the bug it fixes
/// shipped unnoticed until Windows CI hit it.
#[allow(dead_code)] // unix builds compile this but never call it (symlink branch above)
pub(super) fn copy_dir_recursive(src: &std::path::Path, dest: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
