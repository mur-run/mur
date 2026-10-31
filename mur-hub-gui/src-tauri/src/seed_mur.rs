//! Seed the built-in "MUR" agent from the bundled template on first launch.
//!
//! Idempotent: seeds only when an agent named `mur` is not already present, so it
//! never clobbers an existing MUR or a user who deleted MUR on purpose — but,
//! unlike a blanket "directory is empty" check, it still provides the built-in
//! concierge to users who already have other agents.

use std::path::Path;

use mur_common::agent::AUTHORING_DIRS;

/// True if MUR is already seeded, i.e. `<mur_home>/agents/mur/profile.yaml` exists.
/// We key off the profile file (not just the directory) so a previously broken
/// half-seeded `agents/mur` directory is treated as "not seeded" and gets healed.
pub fn mur_seeded(mur_home: &Path) -> bool {
    mur_home
        .join("agents")
        .join("mur")
        .join("profile.yaml")
        .is_file()
}

/// Recursively copy `src` into `dst`.
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Replace the `id:` line in a freshly-staged `profile.yaml` with a new UUIDv7.
///
/// The bundled template carries an all-zeros placeholder so it can stay a static
/// resource. The runtime rejects any profile whose `id` is not a UUIDv7, so each
/// install must mint its own — otherwise the seeded concierge can never start.
fn assign_fresh_profile_id(profile_path: &Path) -> std::io::Result<()> {
    let content = std::fs::read_to_string(profile_path)?;
    let new_id = uuid::Uuid::now_v7();
    let mut replaced = false;
    let mut out = content
        .lines()
        .map(|line| {
            if !replaced && line.trim_start().starts_with("id:") {
                replaced = true;
                let indent = &line[..line.len() - line.trim_start().len()];
                format!("{indent}id: \"{new_id}\"")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }
    std::fs::write(profile_path, out)
}

/// Repair an already-seeded `agents/mur` profile from older/broken builds so it
/// can actually start. Idempotent; returns Ok(true) if it changed anything.
///
/// Fixes the three startup-blockers shipped by earlier templates:
///   1. `id` all-zeros placeholder → a fresh UUIDv7 (runtime requires UUIDv7).
///   2. `unix://PLACEHOLDER/agent.sock` → `unix://{{agent_home}}/agent.sock`
///      (the runtime only expands the `{{agent_home}}` token).
///   3. top-level `name:` → `mur` so it matches the on-disk directory (the
///      runtime's spoof check is an exact string match).
pub fn repair_mur_profile(mur_home: &Path) -> std::io::Result<bool> {
    let profile_path = mur_home.join("agents").join("mur").join("profile.yaml");
    if !profile_path.is_file() {
        return Ok(false);
    }
    let original = std::fs::read_to_string(&profile_path)?;
    let mut out = original.clone();

    if out.contains("00000000-0000-0000-0000-000000000000") {
        out = out.replace(
            "00000000-0000-0000-0000-000000000000",
            &uuid::Uuid::now_v7().to_string(),
        );
    }
    out = out.replace(
        "unix://PLACEHOLDER/agent.sock",
        "unix://{{agent_home}}/agent.sock",
    );
    // Align the top-level `name:` (not `display_name:`) with the dir slug, and
    // normalise the old default `display_name: "Mur"` to the uppercase brand
    // "MUR" (only the stock value — never clobber a user rename).
    out = out
        .lines()
        .map(|l| {
            if l.starts_with("name:") && l["name:".len()..].trim() != "mur" {
                "name: mur".to_string()
            } else if let Some(rest) = l.strip_prefix("display_name:") {
                let v = rest.trim().trim_matches('"');
                if v == "Mur" || v == "MuR" {
                    "display_name: \"MUR\"".to_string()
                } else {
                    l.to_string()
                }
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if original.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }

    if out != original {
        std::fs::write(&profile_path, out)?;
        return Ok(true);
    }
    Ok(false)
}

/// When the bundled MLX model is unavailable, point the stock concierge at a
/// reachable local ollama chat model so it can actually respond out of the box
/// (otherwise the seeded concierge has no working inference backend).
///
/// Scoped to the *stock* `provider: local` concierge model — never overrides a
/// model the user chose. No-op if ollama isn't reachable. Returns Ok(true) if
/// it switched the model.
pub fn ensure_concierge_model(mur_home: &Path) -> std::io::Result<bool> {
    let profile_path = mur_home.join("agents").join("mur").join("profile.yaml");
    if !profile_path.is_file() {
        return Ok(false);
    }
    let original = std::fs::read_to_string(&profile_path)?;
    // Never override an explicit model choice: a `model_ref` points the agent at
    // a registry entry (e.g. a user-configured oMLX / OpenAI endpoint).
    if original.contains("model_ref:") {
        return Ok(false);
    }
    // Only touch the stock local/MLX model — leave any user choice alone.
    if !original.contains("provider: local") {
        return Ok(false);
    }
    let Some(model) = first_ollama_chat_model() else {
        return Ok(false);
    };

    let out = rewrite_model_block(&original, "ollama", &model);
    if out != original {
        std::fs::write(&profile_path, out)?;
        return Ok(true);
    }
    Ok(false)
}

/// The name of a usable local ollama chat model, if ollama is reachable.
/// Prefers known small/fast models; skips embedding-only models.
fn first_ollama_chat_model() -> Option<String> {
    let base =
        std::env::var("OLLAMA_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(900))
        .build()
        .ok()?;
    let json: serde_json::Value = client
        .get(format!("{base}/api/tags"))
        .send()
        .ok()?
        .json()
        .ok()?;
    let names: Vec<String> = json
        .get("models")?
        .as_array()?
        .iter()
        .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(String::from))
        .filter(|n| !n.contains("embed")) // embedding models can't chat
        .collect();
    // Prefer a small, fast general chat model when present.
    for pref in ["qwen3:4b", "llama3.2:3b", "qwen3:8b"] {
        if let Some(n) = names.iter().find(|n| n.as_str() == pref) {
            return Some(n.clone());
        }
    }
    names.into_iter().next()
}

/// Rewrite the `provider:` and `name:` lines inside the top-level `model:` block.
fn rewrite_model_block(yaml: &str, provider: &str, name: &str) -> String {
    let mut in_model = false;
    let mut out: Vec<String> = Vec::new();
    for line in yaml.lines() {
        if line.starts_with("model:") {
            in_model = true;
            out.push(line.to_string());
            continue;
        }
        if in_model {
            let trimmed = line.trim_start();
            let indent = &line[..line.len() - trimmed.len()];
            // A non-indented, non-empty line ends the block.
            if indent.is_empty() && !line.trim().is_empty() {
                in_model = false;
            } else if trimmed.starts_with("provider:") {
                out.push(format!("{indent}provider: {provider}"));
                continue;
            } else if trimmed.starts_with("name:") {
                out.push(format!("{indent}name: {name}"));
                continue;
            }
        }
        out.push(line.to_string());
    }
    let mut joined = out.join("\n");
    if yaml.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// Point the stock concierge at a `~/.mur/models.yaml` registry alias by setting
/// the top-level `model_ref:` field (the runtime prefers it over the inline
/// `model:` block). Returns Ok(true) if the file changed.
pub fn set_concierge_model_ref(mur_home: &Path, alias: &str) -> std::io::Result<bool> {
    let profile_path = mur_home.join("agents").join("mur").join("profile.yaml");
    if !profile_path.is_file() {
        return Ok(false);
    }
    let original = std::fs::read_to_string(&profile_path)?;
    let out = upsert_model_ref(&original, alias);
    if out != original {
        std::fs::write(&profile_path, out)?;
        return Ok(true);
    }
    Ok(false)
}

/// Insert or replace the top-level `model_ref:` line. Replaces an existing line
/// in place; otherwise inserts one immediately before the `model:` block (or
/// appends if neither key exists). A top-level line has no leading indentation.
fn upsert_model_ref(yaml: &str, alias: &str) -> String {
    let has_existing = yaml.lines().any(|l| l.starts_with("model_ref:"));
    let mut out: Vec<String> = Vec::new();
    for line in yaml.lines() {
        if has_existing {
            if line.starts_with("model_ref:") {
                out.push(format!("model_ref: {alias}"));
            } else {
                out.push(line.to_string());
            }
        } else {
            if line.starts_with("model:") {
                out.push(format!("model_ref: {alias}"));
            }
            out.push(line.to_string());
        }
    }
    if !has_existing && !yaml.lines().any(|l| l.starts_with("model:")) {
        out.push(format!("model_ref: {alias}"));
    }
    let mut joined = out.join("\n");
    if yaml.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// Seed MUR from `template_dir` into `<mur_home>/agents/mur` iff MUR is not already
/// seeded. Returns Ok(true) if seeding happened, Ok(false) if skipped.
///
/// The copy is staged in a sibling temp directory and renamed into place so a
/// failure part-way through never leaves a broken `agents/mur`. If a previous run
/// left an empty/broken `agents/mur` (e.g. the template could not be found), it is
/// replaced.
pub fn seed_mur_if_missing(template_dir: &Path, mur_home: &Path) -> std::io::Result<bool> {
    if mur_seeded(mur_home) {
        return Ok(false);
    }
    // Validate the template up-front so we never create a destination dir for a
    // source that does not exist.
    if !template_dir.join("profile.yaml").is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "seed template missing profile.yaml at {}",
                template_dir.display()
            ),
        ));
    }

    let agents = mur_home.join("agents");
    std::fs::create_dir_all(&agents)?;

    // The template grants write on these, but the sandbox drops a grant whose
    // path does not exist at seal time (sandbox/policy/build.rs, Issue 16) — so on a fresh
    // host the concierge would boot with every authoring grant silently gone.
    // Create them here, next to the seed that grants them.
    for d in AUTHORING_DIRS {
        std::fs::create_dir_all(mur_home.join(d))?;
    }
    // Not an authoring grant (the concierge must not write fleet definitions),
    // but the runtime's own read of `fleets/` is existence-checked at seal time
    // too. Created here so the first `mur fleet create` is visible to a Linux
    // (Landlock) concierge without a restart.
    std::fs::create_dir_all(mur_home.join(mur_common::paths::FLEETS))?;

    let staging = agents.join(".mur.seeding");
    let dst = agents.join("mur");

    // Clean any leftover staging from a previous interrupted run.
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    copy_tree(template_dir, &staging)?;

    // The bundled template ships a placeholder `id` (all-zeros) so it stays a
    // static resource. The runtime rejects any profile whose `id` is not a
    // UUIDv7 (`profile.id must be UUIDv7`), so mint a fresh one per install —
    // otherwise the seeded concierge can never start and two-way comms break
    // out of the box.
    assign_fresh_profile_id(&staging.join("profile.yaml"))?;

    // Replace any broken/empty existing dir, then atomically move staging into place.
    if dst.exists() {
        std::fs::remove_dir_all(&dst)?;
    }
    std::fs::rename(&staging, &dst)?;
    // #712: pin the freshly seeded concierge so its first start trusts the
    // template's entitlements rather than whatever a stale pin said.
    // Best-effort: on failure the stale pin is removed instead, and the
    // runtime pins on first start (trust on first use).
    if mur_common::entitlements_pin::repin_from_profile(mur_home, "mur").is_err() {
        let _ = mur_common::entitlements_pin::remove_pin(mur_home, "mur");
    }
    Ok(true)
}

/// For an already-seeded `mur` agent, copy in any bundled template skill dirs
/// missing from `<mur_home>/agents/mur/skills/` and append them to the
/// agent's `profile.yaml` `skills:` list. Existing skill dirs are left
/// completely untouched (never overwritten). Returns the names seeded.
///
/// Called on Hub launch right after `seed_mur_if_missing` — a cheap no-op
/// when nothing is missing, so upgrades pick up new bundled skills (e.g.
/// brainstorming) without touching an agent the user already customized.
pub fn seed_missing_bundled_skills(
    template_dir: &Path,
    mur_home: &Path,
) -> std::io::Result<Vec<String>> {
    let agent_dir = mur_home.join("agents").join("mur");
    let template_skills = template_dir.join("skills");
    if !agent_dir.join("profile.yaml").is_file() || !template_skills.is_dir() {
        return Ok(vec![]);
    }

    let profile_path = agent_dir.join("profile.yaml");
    let mut profile = std::fs::read_to_string(&profile_path)?;
    let mut seeded = Vec::new();

    for entry in std::fs::read_dir(&template_skills)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let dst = agent_dir.join("skills").join(&name);
        let was_missing = !dst.exists();
        if was_missing {
            copy_tree(&entry.path(), &dst)?;
        }
        let reference = format!("skills/{name}");
        if !profile
            .lines()
            .any(|l| l.trim() == format!("- {reference}"))
        {
            profile = append_skill_reference(&profile, &reference);
        }
        if was_missing {
            seeded.push(name);
        }
    }

    std::fs::write(&profile_path, profile)?;
    Ok(seeded)
}

/// Append `- <reference>` as the last entry of the `skills:` list in `profile`.
///
/// This edits the *user's* live profile.yaml textually (to preserve their
/// formatting), so it must accept every form the key can take and never
/// panic: block lists, inline flow lists (`skills: []`, `skills: [a, b]` —
/// converted to block style), and a missing key (a section is appended).
fn append_skill_reference(profile: &str, reference: &str) -> String {
    let mut lines: Vec<String> = profile.lines().map(str::to_owned).collect();
    let new_entry = format!("  - {reference}");

    if let Some(idx) = lines.iter().position(|l| {
        let t = l.trim_end();
        t == "skills:" || t.starts_with("skills: ") || t.starts_with("skills:\t")
    }) {
        let rest = lines[idx].trim_end()["skills:".len()..].trim().to_owned();
        if !rest.is_empty() {
            // Inline flow list: convert to block style, keeping existing entries.
            let items: Vec<String> = rest
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
            if items.iter().any(|i| i == reference) {
                return profile.to_owned();
            }
            lines[idx] = "skills:".to_owned();
            for (offset, item) in items.iter().enumerate() {
                lines.insert(idx + 1 + offset, format!("  - {item}"));
            }
        }
        let mut insert_at = idx + 1;
        while insert_at < lines.len() && lines[insert_at].trim_start().starts_with("- ") {
            insert_at += 1;
        }
        lines.insert(insert_at, new_entry);
    } else {
        lines.push("skills:".to_owned());
        lines.push(new_entry);
    }

    let mut out = lines.join("\n");
    if profile.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests;
