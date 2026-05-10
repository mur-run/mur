// Stub — fully implemented in M-c7.2+

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
