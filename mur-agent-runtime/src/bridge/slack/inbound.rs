// Stub — fully implemented in M-c7.2+
use crate::bridge::slack::SlackError;

pub trait SlackBotLike: Send + Sync + 'static {}

pub struct InboundDeps;

pub struct SlackInboundLoop<B: SlackBotLike> {
    pub bot: B,
    pub(crate) deps: Option<InboundDeps>,
}

impl<B: SlackBotLike> SlackInboundLoop<B> {
    pub fn stub_new(bot: B) -> Self {
        Self { bot, deps: None }
    }
}

// Suppress unused-import warning until M-c7.2 fills this module.
const _: fn() = || {
    let _: Option<SlackError> = None;
};
