//! The conversation channel of the turn being run, for tools that must
//! remember where a request came from (#2464).
//!
//! `remind` is the one reader today: a reminder asked for in a chat is
//! delivered back to that chat when it fires. Bound around the turn's
//! generation like `consult_scope`, so two overlapping turns on one runner
//! each see their own channel.

use std::future::Future;

tokio::task_local! {
    static CURRENT: Option<String>;
}

/// Run `fut` with `channel` as the current turn's conversation.
pub(crate) async fn scope<F: Future>(channel: Option<String>, fut: F) -> F::Output {
    CURRENT.scope(channel, fut).await
}

/// The channel id of the turn whose tool call is executing, if it has one.
pub(crate) fn current() -> Option<String> {
    CURRENT.try_with(Clone::clone).ok().flatten()
}
