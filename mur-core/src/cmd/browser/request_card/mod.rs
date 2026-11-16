//! A pending `browser_request`'s card (#2360 slice 3, S:§The card): which
//! saved logins match the site, what each would do to the agent's live
//! entry, and the answer that writes it. murmur, Hub (slice 4) and
//! `mur browser answer` all go through here, so the cards cannot drift.

// Until Task 5 wires the CLI nothing outside the tests calls in here. The
// attributes cover both lints this raises: `dead_code` on the items and
// `unused_imports` on the re-exports (unused in test builds too, since the
// tests reach the items through `super::*`). Removed in Task 5.
#![cfg_attr(not(test), allow(dead_code))]
#![allow(unused_imports)]

mod model;

pub(crate) use model::{
    Card, Inputs, LoginAgain, Offer, Profile, ProfileRow, Reach, add_hosts, build, offer_for,
    port_step,
};
