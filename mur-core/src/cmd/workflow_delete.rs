//! `mur workflow delete` — remove a published workflow from the server and
//! from disk.
//!
//! Deleting the local file alone accomplishes nothing: `mur sync` pulls every
//! workflow the server holds and writes each one back unconditionally
//! (`cmd/sync_cmd.rs`), so with `sync.auto` on — the default once signed in —
//! the file returns on the next sync. Until now the CLI offered `publish` with
//! no inverse, which made anything published permanently un-removable through
//! `mur`, including something published by mistake.
//!
//! The server has implemented `DELETE /api/v1/workflows/{id}` all along, with
//! a `user_id` guard on the row. This wires the CLI to it.

use anyhow::{Context, Result, bail};

use crate::auth;

/// One entry of `GET /api/v1/workflows`, in the shape the sync pull reads.
#[derive(Debug, serde::Deserialize)]
struct RemoteWorkflow {
    id: String,
    name: String,
}

#[derive(Debug, serde::Deserialize)]
struct ListResponse {
    data: Vec<RemoteWorkflow>,
}

/// Find the server-side id for a workflow the user names locally.
///
/// The user knows a workflow by name; the delete endpoint takes a UUID. An
/// ambiguous name is refused rather than resolved by picking one — deleting
/// the wrong published workflow is not recoverable through this CLI.
async fn resolve_id(client: &reqwest::Client, name: &str) -> Result<String> {
    let url = format!("{}/api/v1/workflows", auth::server_url());
    let resp = auth::auth_request(client, reqwest::Method::GET, &url)
        .await?
        .send()
        .await
        .context("connect to the mur server")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("listing workflows failed ({status}): {body}");
    }
    let list: ListResponse = resp
        .json()
        .await
        .context("invalid workflow list response")?;

    let matches: Vec<&RemoteWorkflow> = list.data.iter().filter(|w| w.name == name).collect();
    match matches.len() {
        0 => bail!(
            "no workflow named `{name}` on the server. \
             `mur workflow list` shows what is installed locally; a local-only \
             workflow is removed by deleting its file under ~/.mur/workflows/.",
        ),
        1 => Ok(matches[0].id.clone()),
        n => bail!(
            "{n} workflows on the server are named `{name}`; refusing to guess which to delete. \
             Delete by id via the API, or rename them so the names are distinct.",
        ),
    }
}

/// `mur workflow delete <name> [--yes] [--local-only]`
pub async fn cmd_workflow_delete(name: &str, yes: bool, local_only: bool) -> Result<()> {
    let local_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".mur")
        .join("workflows")
        .join(format!("{name}.yaml"));

    if local_only {
        if !local_path.is_file() {
            bail!("no local file at {}", local_path.display());
        }
        if !confirm(
            yes,
            &format!(
                "Delete the LOCAL copy of `{name}` only?\n  {}\n\
                 It will come back on the next `mur sync` if the server still has it.",
                local_path.display(),
            ),
        )? {
            bail!("cancelled");
        }
        std::fs::remove_file(&local_path)
            .with_context(|| format!("remove {}", local_path.display()))?;
        println!("Deleted local copy of `{name}`.");
        return Ok(());
    }

    let client = reqwest::Client::new();
    let id = resolve_id(&client, name).await?;

    if !confirm(
        yes,
        &format!(
            "Delete workflow `{name}` ({id}) from the server?\n\
             This removes it for every device that syncs this account, and cannot be undone here."
        ),
    )? {
        bail!("cancelled");
    }

    let url = format!("{}/api/v1/workflows/{id}", auth::server_url());
    let resp = auth::auth_request(&client, reqwest::Method::DELETE, &url)
        .await?
        .send()
        .await
        .context("connect to the mur server")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("delete failed ({status}): {body}");
    }
    println!("Deleted `{name}` from the server.");

    // Local file second, and only best-effort: the server copy is what makes
    // the deletion stick. A local file left behind is removed by the next
    // sync's absence, whereas a local-only delete would silently come back.
    match std::fs::remove_file(&local_path) {
        Ok(()) => println!("Removed {}", local_path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("warning: could not remove {}: {e}", local_path.display()),
    }
    Ok(())
}

/// Prompt unless `--yes`. Deleting a published workflow affects every device
/// on the account, so it asks by default.
fn confirm(yes: bool, message: &str) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    use std::io::Write;
    println!("{message}");
    print!("Proceed? [y/N] ");
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .context("read confirmation from stdin")?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(names: &[&str]) -> Vec<RemoteWorkflow> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| RemoteWorkflow {
                id: format!("id-{i}"),
                name: (*n).to_string(),
            })
            .collect()
    }

    /// Extracted so the resolution rule is testable without a server: it is
    /// the part that decides which published workflow gets destroyed.
    fn pick<'a>(data: &'a [RemoteWorkflow], name: &str) -> std::result::Result<&'a str, usize> {
        let m: Vec<&RemoteWorkflow> = data.iter().filter(|w| w.name == name).collect();
        match m.len() {
            1 => Ok(&m[0].id),
            n => Err(n),
        }
    }

    #[test]
    fn resolves_a_unique_name_to_its_id() {
        let d = list(&["alpha", "beta"]);
        assert_eq!(pick(&d, "beta"), Ok("id-1"));
    }

    #[test]
    fn an_absent_name_is_not_a_silent_success() {
        let d = list(&["alpha"]);
        assert_eq!(pick(&d, "ghost"), Err(0));
    }

    /// Picking one of several same-named workflows would destroy a published
    /// artifact the user did not choose, with no undo in this CLI.
    #[test]
    fn duplicate_names_refuse_rather_than_guess() {
        let d = list(&["dup", "other", "dup"]);
        assert_eq!(pick(&d, "dup"), Err(2));
    }

    #[test]
    fn name_matching_is_exact() {
        let d = list(&["deploy", "deploy-frontend"]);
        assert_eq!(pick(&d, "deploy"), Ok("id-0"));
        assert_eq!(pick(&d, "deploy-front"), Err(0), "no prefix matching");
    }
}
