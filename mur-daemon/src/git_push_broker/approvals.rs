//! The approval loop of the git push broker (spec §4).
//!
//! Each pass does three things, in this order:
//!
//! 1. every row the broker holds as `pending_approval` and that has no `HitlRequest` yet gets one,
//!    signed by the router and pinned to the row's `action_hash`;
//! 2. the channel's events are folded by [`HumanApprovalVerifier`], the only judge of who may
//!    approve;
//! 3. an allow goes to `Broker::on_approval`, a deny to `Broker::on_denial`, and the agent's status
//!    file is rewritten with the row's resulting state.
//!
//! Nothing here ages a row out. A request with no answer stays pending across passes, and a
//! decision the broker cannot act on yet (unreadable enrollments) leaves the row pending and the
//! approval unspent, so the next pass tries again.

use std::path::Path;

use mur_channel::ChannelService;
use mur_common::channel::{
    CHANNEL_SCHEMA_VERSION, Channel, ChannelActor, ChannelEvent, ChannelPurpose, ChannelState,
    EventKind, Goal,
};
use mur_common::config::Config;
use mur_common::git_push::private_dir;
use mur_common::hitl::{HitlRequest, RiskTier};
use mur_core::channel_writer::{ROUTER_AGENT, append_as_writer};
use mur_core::hitl::authority::is_router_signed;
use mur_git_broker::action::ActionDocument;
use mur_git_broker::pending::{RequestKey, Row, State};

use super::HumanApprovalVerifier;
use super::intake::{BrokerSlot, PENDING_DB, publish};

/// The one channel the broker's approval requests live on. A fixed id, so the CLI, the Hub and the
/// phone all find the same list, and a restart does not strand an open question.
pub const BROKER_CHANNEL_ID: &str = "git-push-broker";
const TOOL_NAME: &str = "git_push";
/// The broker never times a pending row out (spec §4); the request carries no deadline either.
const NO_TIMEOUT_MS: u64 = 0;
const IDEMPOTENCY_PREFIX: &str = "git-push-request:";

/// One approval pass. A no-op while `git_push.enabled` is off or before any row was ever stored.
pub fn approval_pass(mur_home: &Path, slot: &BrokerSlot) {
    if !Config::load_or_default(&mur_home.join("config.yaml"))
        .git_push
        .enabled
    {
        return;
    }
    // Nothing was ever submitted: do not create the broker's private directory for an empty loop.
    if !private_dir(mur_home).join(PENDING_DB).exists() {
        return;
    }
    let broker = match slot.get(mur_home) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "git push approvals: broker unavailable");
            return;
        }
    };
    let rows = match broker.store().list_pending() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "git push approvals: pending rows unreadable");
            return;
        }
    };
    if rows.is_empty() {
        return;
    }
    let events = match open_requests(mur_home, &rows) {
        Ok(ev) => ev,
        Err(e) => {
            tracing::warn!(error = %e, "git push approvals: channel unavailable");
            return;
        }
    };
    let pending: Vec<(RequestKey, String)> = rows
        .iter()
        .map(|r| (r.key.clone(), r.action_hash.clone()))
        .collect();
    let verifier = HumanApprovalVerifier::new(mur_home, BROKER_CHANNEL_ID);
    for (folded, allow) in verifier.decide(&events, &pending) {
        let key = &folded.key;
        let outcome = if allow {
            broker.on_approval(key, folded.proof.clone()).map(|_| ())
        } else {
            broker.on_denial(key)
        };
        let code = outcome.as_ref().err().map(|e| e.code());
        match &outcome {
            Ok(()) => {
                tracing::info!(agent = %key.agent_id, request_id = %key.request_id, allow, "git push decision applied")
            }
            Err(e) => {
                tracing::warn!(agent = %key.agent_id, request_id = %key.request_id, allow, code = e.code(), "git push decision not applied")
            }
        }
        // The row is the truth about where the request ended up, whatever the call returned.
        let Ok(Some(row)) = broker.store().get(key) else {
            continue;
        };
        if row.state == State::PendingApproval {
            // Not acted on yet (for example the records were unreadable); say nothing new.
            continue;
        }
        if let Err(e) = publish(
            mur_home,
            &key.agent_id,
            &key.request_id,
            row.state.as_str(),
            code,
            Some(&row.action_hash),
        ) {
            tracing::warn!(agent = %key.agent_id, request_id = %key.request_id, error = %e, "git push status not written");
        }
    }
}

/// Make sure the channel exists and every row has its request on it. Returns the channel's events,
/// including the requests just written.
fn open_requests(mur_home: &Path, rows: &[Row]) -> anyhow::Result<Vec<ChannelEvent>> {
    let svc = ChannelService::open(mur_home)?;
    if !svc.exists(BROKER_CHANNEL_ID) {
        create_channel(&svc)?;
    }
    let mut events = svc.load_events(BROKER_CHANNEL_ID)?;
    for row in rows {
        let id = &row.key.request_id;
        if has_request(&events, id) {
            continue;
        }
        let ev = append_as_writer(
            &svc,
            mur_home,
            BROKER_CHANNEL_ID,
            ROUTER_AGENT,
            ChannelActor::System,
            EventKind::HitlRequest,
            serde_json::to_value(request_for(row)?)?,
            Some(format!("{IDEMPOTENCY_PREFIX}{id}")),
        )?;
        if !is_router_signed(mur_home, BROKER_CHANNEL_ID, &ev) {
            // Written, but no human answer can bind to it: the gate only counts router-signed
            // requests. Say so now rather than leave a question nobody can settle.
            tracing::warn!(
                request_id = %id,
                router = ROUTER_AGENT,
                "git push approval request is not router-signed; it cannot be approved until the router identity exists"
            );
        }
        events.push(ev);
    }
    Ok(events)
}

fn has_request(events: &[ChannelEvent], hitl_id: &str) -> bool {
    events
        .iter()
        .filter(|e| e.kind == EventKind::HitlRequest)
        .filter_map(|e| serde_json::from_value::<HitlRequest>(e.payload.clone()).ok())
        .any(|r| r.hitl_id == hitl_id)
}

/// The request a human sees. `hitl_id` is the broker's `request_id`: that is the id the fold
/// matches a response on, and `action_hash` is the row's, so an answer can only mean this action.
fn request_for(row: &Row) -> anyhow::Result<HitlRequest> {
    let doc: ActionDocument = serde_json::from_str(&row.doc_json)?;
    let update = doc
        .updates
        .first()
        .ok_or_else(|| anyhow::anyhow!("pending action has no ref update"))?;
    Ok(HitlRequest {
        hitl_id: row.key.request_id.clone(),
        action_hash: row.action_hash.clone(),
        tier: RiskTier::NetworkEgress,
        tool_name: TOOL_NAME.into(),
        tool_input: serde_json::to_value(&doc)?,
        step_or_call_id: row.key.request_id.clone(),
        agent_id: row.key.agent_id.clone(),
        timeout_ms: NO_TIMEOUT_MS,
        summary: format!(
            "{agent} pushes {new} to {r} on {remote} (was {old})",
            agent = doc.agent_id,
            new = update.new_sha,
            r = update.r#ref,
            remote = doc.canonical_remote_endpoint,
            old = update.old_sha,
        ),
        issued_at: Some(chrono::Utc::now()),
    })
}

fn create_channel(svc: &ChannelService) -> anyhow::Result<()> {
    let now = chrono::Utc::now();
    let ch = Channel {
        v: CHANNEL_SCHEMA_VERSION,
        id: BROKER_CHANNEL_ID.into(),
        title: "git push approvals".into(),
        goal: Goal::default(),
        state: ChannelState::Working,
        purpose: Some(ChannelPurpose::WorkflowRun),
        owner: ChannelActor::local_human(),
        participants: vec![],
        created_at: now,
        updated_at: now,
    };
    svc.store().create(&ch)?;
    svc.index().upsert(&ch)?;
    Ok(())
}
