//! A pending `browser_request`'s card (#2360 slice 3, S:§The card): which
//! saved logins match the site, what each would do to the agent's live
//! entry, and the answer that writes it. murmur, Hub (slice 4) and
//! `mur browser answer` all go through here, so the cards cannot drift.

mod model;

pub(crate) use model::{Card, Offer, Profile, Reach, offer_for};
#[cfg(test)]
pub(crate) use model::{LoginAgain, ProfileRow};

mod apply;

pub(crate) use apply::{Answer, Answered, Choice, answer, use_offer};

mod login;

pub(crate) use login::{login, login_plan};

mod ops;

pub(crate) use ops::{AgentPorts, card_for, open_requests, shared_with};
