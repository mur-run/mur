//! `mur drafts` — list / show / accept / reject pending pattern drafts.
//!
//! Drafts are server-side proposals (Channel 2/3) created from Signals whose
//! target is `NewDraftPattern`. Unlike signals, drafts are NOT routed through
//! the shared Inbox; they are queried on-demand via this subcommand tree.
//!
//! Endpoints:
//! - `GET  /api/v1/core/drafts/pending?since=&limit=`
//! - `POST /api/v1/core/drafts/{id}/reject`
//!
//! See also: `sync/client.rs` for `fetch_drafts` / `reject_draft`.

use anyhow::{Context, Result, anyhow};
use chrono::{Duration, Utc};
use mur_common::pattern::Pattern;

use crate::store::yaml::YamlStore;
use crate::sync::{DraftRecord, SyncClient};

/// Page size for `fetch_drafts` pagination. Chosen so a typical user can
/// dump a month of inbox in 1–2 round trips.
const DEFAULT_PAGE_LIMIT: u32 = 100;

fn client_from_env() -> Result<SyncClient> {
    let tokens =
        crate::auth::load_tokens().ok_or_else(|| anyhow!("not logged in (run `mur login`)"))?;
    let url = crate::auth::server_url();
    SyncClient::new(url, tokens.access_token)
}

/// Fetch all pending drafts, paginating via `next_cursor` until exhausted
/// or the server returns an empty page. Optionally filter to drafts created
/// within the last `since_days` days (client-side; server currently returns
/// all pending regardless of cursor age).
async fn fetch_all_pending(client: &SyncClient, since_days: u32) -> Result<Vec<DraftRecord>> {
    let cutoff = Utc::now() - Duration::days(since_days as i64);
    let mut out: Vec<DraftRecord> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let resp = client
            .fetch_drafts(cursor.as_deref(), DEFAULT_PAGE_LIMIT)
            .await
            .context("fetch_drafts")?;
        let got = resp.drafts.len();
        for d in resp.drafts {
            if d.created_at >= cutoff {
                out.push(d);
            }
        }
        match resp.next_cursor {
            Some(c) if !c.is_empty() && got > 0 => cursor = Some(c),
            _ => break,
        }
    }
    Ok(out)
}

/// Render the short prefix of a uuid used in the table / prefix-match.
fn short_id(u: &uuid::Uuid) -> String {
    u.to_string().chars().take(8).collect()
}

/// Render the scope field compactly for table output.
fn scope_label(scope: &mur_common::Scope) -> String {
    match scope {
        mur_common::Scope::Personal => "personal".to_string(),
        mur_common::Scope::Team { team_id } => format!("team:{team_id}"),
        mur_common::Scope::Community { pack_id } => match pack_id {
            Some(p) => format!("community:{p}"),
            None => "community".to_string(),
        },
    }
}

/// `mur drafts list [--since DAYS]`
pub(crate) async fn cmd_drafts_list(since_days: u32) -> Result<()> {
    let client = client_from_env()?;
    let drafts = fetch_all_pending(&client, since_days).await?;

    if drafts.is_empty() {
        println!("no drafts");
        return Ok(());
    }

    println!(
        "{:<10} {:<20} {:<16} {:<8} {:<10} NAME",
        "ID", "CREATED", "SCOPE", "STATUS", "CONFIDENCE"
    );
    for d in &drafts {
        println!(
            "{:<10} {:<20} {:<16} {:<8} {:<10.2} {}",
            short_id(&d.id),
            d.created_at.format("%Y-%m-%d %H:%M:%S"),
            scope_label(&d.scope),
            d.status,
            d.confidence,
            d.payload.name
        );
    }
    Ok(())
}

/// Resolve a user-supplied id prefix against a slice of drafts.
/// Ambiguity or no-match is a hard error (stable messages so integration
/// tests + docs can match on them).
fn resolve_prefix<'a>(prefix: &str, drafts: &'a [DraftRecord]) -> Result<&'a DraftRecord> {
    let matches: Vec<&DraftRecord> = drafts
        .iter()
        .filter(|d| d.id.to_string().starts_with(prefix))
        .collect();
    match matches.len() {
        0 => Err(anyhow!("no pending draft with id prefix '{prefix}'")),
        1 => Ok(matches[0]),
        n => Err(anyhow!(
            "ambiguous id prefix '{prefix}' — matched {n} drafts"
        )),
    }
}

/// `mur drafts show <id-prefix>`
pub(crate) async fn cmd_drafts_show(prefix: &str) -> Result<()> {
    let client = client_from_env()?;
    let drafts = fetch_all_pending(&client, 365).await?; // wide net for show
    let d = resolve_prefix(prefix, &drafts)?;

    println!("id:             {}", d.id);
    println!("created_at:     {}", d.created_at);
    println!("status:         {}", d.status);
    println!("scope:          {}", scope_label(&d.scope));
    println!("confidence:     {:.2}", d.confidence);
    println!("origin_context: {}", d.origin_context);
    println!();
    println!("---  pattern payload  ---");
    println!(
        "{}",
        serde_yaml::to_string(&d.payload).context("serialize pattern")?
    );
    Ok(())
}

/// `mur drafts accept <id-prefix> [--as-tier <tier>]`
///
/// Takes the embedded Pattern, promotes its maturity to `Emerging` (the
/// acceptance itself is the human gate), optionally overrides tier, saves
/// to `~/.mur/patterns/<name>.yaml` via [`YamlStore`], and prints a hint
/// to run `mur reindex`.
///
/// **Known limitation (MVP):** accept does NOT notify the server. The next
/// `mur drafts list` call will still surface this draft as pending. Use
/// `mur drafts reject --reason accepted_locally` to hide it, or wait for
/// a follow-up that adds a server-side accept endpoint.
pub(crate) async fn cmd_drafts_accept(prefix: &str, as_tier: Option<&str>) -> Result<()> {
    let client = client_from_env()?;
    let drafts = fetch_all_pending(&client, 365).await?;
    let d = resolve_prefix(prefix, &drafts)?;

    let mut pattern: Pattern = d.payload.clone();
    pattern.base.maturity = mur_common::knowledge::Maturity::Emerging;

    if let Some(t) = as_tier {
        pattern.base.tier = match t {
            "session" => mur_common::pattern::Tier::Session,
            "project" => mur_common::pattern::Tier::Project,
            "core" => mur_common::pattern::Tier::Core,
            other => {
                return Err(anyhow!(
                    "unknown tier '{other}' (want session|project|core)"
                ));
            }
        };
    }

    let store = YamlStore::default_store()?;
    store.save(&pattern).context("save accepted pattern")?;

    println!(
        "accepted draft {} -> ~/.mur/patterns/{}.yaml (maturity=emerging)",
        short_id(&d.id),
        pattern.name
    );
    println!("run `mur reindex` to update the vector index");
    // TODO: once the server grows an /accept endpoint (or we repurpose
    // reject with a sentinel reason), notify the server so this draft
    // stops appearing in `drafts list`. For now accept is local-only.
    Ok(())
}

/// `mur drafts reject <id-prefix> [--reason "..."]`
pub(crate) async fn cmd_drafts_reject(prefix: &str, reason: Option<&str>) -> Result<()> {
    let client = client_from_env()?;
    let drafts = fetch_all_pending(&client, 365).await?;
    let d = resolve_prefix(prefix, &drafts)?;
    client
        .reject_draft(d.id, reason)
        .await
        .context("reject_draft")?;
    println!("rejected draft {}", short_id(&d.id));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_draft(id_hex: &str, name: &str) -> DraftRecord {
        let id = uuid::Uuid::parse_str(id_hex).unwrap();
        DraftRecord {
            id,
            scope: mur_common::Scope::Personal,
            payload: Pattern {
                base: mur_common::knowledge::KnowledgeBase {
                    name: name.into(),
                    description: "x".into(),
                    content: mur_common::pattern::Content::Plain("x".into()),
                    ..Default::default()
                },
                kind: None,
                origin: None,
                attachments: vec![],
            },
            origin_context: "".into(),
            confidence: 1.0,
            status: "pending".into(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn resolve_prefix_unique_match() {
        let drafts = vec![
            make_draft("11111111-1111-1111-1111-111111111111", "a"),
            make_draft("22222222-2222-2222-2222-222222222222", "b"),
        ];
        let r = resolve_prefix("1111", &drafts).unwrap();
        assert_eq!(r.payload.name, "a");
    }

    #[test]
    fn resolve_prefix_ambiguous_errors() {
        let drafts = vec![
            make_draft("11111111-1111-1111-1111-111111111111", "a"),
            make_draft("11112222-2222-2222-2222-222222222222", "b"),
        ];
        let err = resolve_prefix("1111", &drafts).unwrap_err();
        assert!(format!("{err}").contains("ambiguous"));
    }

    #[test]
    fn resolve_prefix_no_match_errors() {
        let drafts = vec![make_draft("11111111-1111-1111-1111-111111111111", "a")];
        let err = resolve_prefix("abcd", &drafts).unwrap_err();
        assert!(format!("{err}").contains("no pending draft"));
    }

    #[test]
    fn scope_label_matches_expected_formats() {
        assert_eq!(scope_label(&mur_common::Scope::Personal), "personal");
        assert_eq!(
            scope_label(&mur_common::Scope::Team {
                team_id: "ops".into()
            }),
            "team:ops"
        );
        assert_eq!(
            scope_label(&mur_common::Scope::Community {
                pack_id: Some("rs".into())
            }),
            "community:rs"
        );
    }
}
