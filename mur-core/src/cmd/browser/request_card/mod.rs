//! A pending `browser_request`'s card (#2360 slice 3, S:§The card): which
//! saved logins match the site, what each would do to the agent's live
//! entry, and the answer that writes it. murmur, Hub (slice 4) and
//! `mur browser answer` all go through here, so the cards cannot drift.

// Until Task 5 wires the CLI nothing outside the tests calls in here. The
// attributes cover both lints this raises: `dead_code` on the items and
// `unused_imports` on the re-exports (unused in test builds too, since the
// tests reach the items through `super::*`). `dead_code` is unconditional
// because the card login (`login::login`) has no test caller either.
// Removed in Task 5.
#![allow(dead_code)]
#![allow(unused_imports)]

mod model;

pub(crate) use model::{
    Card, Inputs, LoginAgain, Offer, Profile, ProfileRow, Reach, add_hosts, build, offer_for,
    port_step,
};

mod apply;

pub(crate) use apply::{Answer, Answered, Choice, PortOps, answer, hand_back_text, use_offer};

mod login;

pub(crate) use login::{LoginPlan, default_name, login, login_plan};
