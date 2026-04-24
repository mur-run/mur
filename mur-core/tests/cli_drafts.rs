//! Integration tests for the `mur drafts` subcommand tree.
//!
//! These tests spin up a local wiremock server, write a fake `auth.json`
//! under a tmp `$HOME`, and drive the CLI end-to-end. They do NOT assert
//! the full wire format of outgoing requests (those are unit-tested in
//! `mur-core/src/sync/client.rs`); here we verify the plumbing:
//! env → CLI → SyncClient → server → stdout.

use std::path::Path;
use std::process::Command;

use serde_json::json;
use uuid::Uuid;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Tmp-rooted env shared by every test: pins `MUR_HOME`, `HOME`,
/// `USERPROFILE`, and `MUR_SERVER_URL`. Also writes a fake `auth.json`
/// so `crate::auth::load_tokens()` returns `Some`.
fn setup_env(tmp: &Path, server_url: &str) {
    let mur_home = tmp.join(".mur");
    std::fs::create_dir_all(&mur_home).expect("mkdir .mur");
    std::fs::write(
        mur_home.join("auth.json"),
        json!({
            "access_token": "TEST-TOKEN",
            "refresh_token": "",
            "token_type": "Bearer",
            "expires_in": 86400,
            "user_id": "test-user"
        })
        .to_string(),
    )
    .expect("write auth.json");
    // Also write a minimal config.yaml pointing server.url at the mock —
    // some code paths (not the drafts client, but dependencies) still read
    // config. auth::server_url() prefers MUR_SERVER_URL so we don't strictly
    // need this, but it keeps the test hermetic.
    std::fs::write(
        mur_home.join("config.yaml"),
        format!("server:\n  url: {server_url}\n"),
    )
    .expect("write config.yaml");
}

/// Attach pinned env vars to a Command. Mirrors `with_mur_home` in
/// cli_conversations.rs but also sets `MUR_SERVER_URL`.
fn mur_cmd(tmp: &Path, server_url: &str, args: &[&str]) -> Command {
    let mur_home = tmp.join(".mur");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mur"));
    cmd.args(args)
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp)
        .env("USERPROFILE", tmp)
        .env("MUR_SERVER_URL", server_url);
    cmd
}

fn sample_draft_json(id: Uuid, name: &str) -> serde_json::Value {
    json!({
        "id": id,
        "signal_id": Uuid::new_v4(),
        "actor_user_id": Uuid::new_v4(),
        "scope": {"kind": "team", "team_id": "ops"},
        "source_actor": {
            "source": "Slack",
            "native_id": "U1",
            "display_name": null,
            "resolved_user_id": null
        },
        "payload": {
            "schema": 2,
            "name": name,
            "description": "draft description",
            "content": "do X when Y",
            "tier": "project"
        },
        "origin_context": "#eng thread",
        "confidence": 0.87,
        "status": "pending",
        "created_at": "2026-04-22T10:30:00Z",
        "reviewed_at": null
    })
}

#[tokio::test]
async fn mur_drafts_list_empty_prints_no_drafts() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    setup_env(tmp.path(), &server.uri());

    Mock::given(method("GET"))
        .and(path("/api/v1/core/drafts/pending"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "drafts": [],
            "next_cursor": null
        })))
        .mount(&server)
        .await;

    let out = mur_cmd(tmp.path(), &server.uri(), &["drafts", "list"])
        .output()
        .expect("run mur drafts list");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no drafts"),
        "expected 'no drafts'; got: {stdout}"
    );
}

#[tokio::test]
async fn mur_drafts_list_paginates_across_cursor_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    setup_env(tmp.path(), &server.uri());

    let id1 = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let id2 = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();

    // Page 1: no `since` → 2 drafts + next_cursor=foo. We must explicitly
    // require the `since` query param to be absent — otherwise this mock
    // also matches the page-2 request (which has both limit=100 AND
    // since=foo) and the client loops forever because every page hands
    // back next_cursor=foo.
    Mock::given(method("GET"))
        .and(path("/api/v1/core/drafts/pending"))
        .and(query_param("limit", "100"))
        .and(query_param_is_missing("since"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "drafts": [sample_draft_json(id1, "pat-one"), sample_draft_json(id2, "pat-two")],
            "next_cursor": "foo"
        })))
        .mount(&server)
        .await;

    // Page 2: since=foo → empty + next_cursor=null
    Mock::given(method("GET"))
        .and(path("/api/v1/core/drafts/pending"))
        .and(query_param("since", "foo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "drafts": [],
            "next_cursor": null
        })))
        .mount(&server)
        .await;

    // Use a very generous --since so the 2026-04-22 fixture isn't filtered.
    let out = mur_cmd(
        tmp.path(),
        &server.uri(),
        &["drafts", "list", "--since", "3650"],
    )
    .output()
    .expect("run mur drafts list");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("pat-one"), "stdout: {stdout}");
    assert!(stdout.contains("pat-two"), "stdout: {stdout}");
    assert!(stdout.contains("team:ops"), "stdout: {stdout}");
}

#[tokio::test]
async fn mur_drafts_reject_happy_path() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    setup_env(tmp.path(), &server.uri());

    let id = Uuid::parse_str("abc12345-0000-0000-0000-000000000000").unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v1/core/drafts/pending"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "drafts": [sample_draft_json(id, "use-pnpm")],
            "next_cursor": null
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path(format!("/api/v1/core/drafts/{id}/reject")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let out = mur_cmd(
        tmp.path(),
        &server.uri(),
        &["drafts", "reject", "abc12345", "--reason", "not useful"],
    )
    .output()
    .expect("run mur drafts reject");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("rejected draft"),
        "expected confirmation; got: {stdout}"
    );
}

#[tokio::test]
async fn mur_drafts_show_resolves_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    setup_env(tmp.path(), &server.uri());

    let id = Uuid::parse_str("abc12345-0000-0000-0000-000000000000").unwrap();
    Mock::given(method("GET"))
        .and(path("/api/v1/core/drafts/pending"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "drafts": [sample_draft_json(id, "use-pnpm")],
            "next_cursor": null
        })))
        .mount(&server)
        .await;

    let out = mur_cmd(tmp.path(), &server.uri(), &["drafts", "show", "abc1"])
        .output()
        .expect("run mur drafts show");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The full id and the name from the embedded Pattern payload must both
    // appear (we pretty-print the YAML).
    assert!(stdout.contains(&id.to_string()), "stdout: {stdout}");
    assert!(
        stdout.contains("name: use-pnpm"),
        "expected Pattern YAML; got: {stdout}"
    );
    assert!(stdout.contains("origin_context"), "stdout: {stdout}");
}
