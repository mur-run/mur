//! murmur `@` (at-mention C3, assumption S2). A pane is bound to one agent,
//! whose key signs the channel's events, so murmur only delivers to that
//! agent: `@<pane agent>#N text` switches to conversation `N` and sends
//! `text`; `@<pane agent> text` sends `text` in the current channel. Any
//! other target is pointed at `mur agent send`, which writes as the target.

use super::App;
use super::mention::parse_mention;
use super::mention_target::{TargetError, conversation_at, resolve_mention};

/// Route a submitted line. `Some(text)` = send `text` as the turn (the line
/// unchanged when it is not a mention; empty after a bare `@a#N` switch);
/// `None` = handled here with a notice or refusal, nothing to send.
pub(super) fn route(app: &mut App, line: &str) -> Option<String> {
    let Some(parsed) = parse_mention(line) else {
        return Some(line.to_string());
    };
    let m = match parsed {
        Ok(m) => m,
        Err(e) => {
            app.push_error(TargetError::from(e).to_string());
            return None;
        }
    };
    let agent = match resolve_mention(&app.home, &m) {
        Ok(a) => a,
        Err(e) => {
            app.push_error(e.to_string());
            return None;
        }
    };
    if agent != app.agent {
        let n = m.ordinal.map(|n| format!("#{n}")).unwrap_or_default();
        app.push_system(format!(
            "use: mur agent send '@{agent}{n}' … — this pane is bound to {}",
            app.agent
        ));
        app.clear_input();
        return None;
    }
    if let Some(n) = m.ordinal
        && !switch_to(app, n)
    {
        return None;
    }
    // Empty rest after a `#N` = a bare switch; the caller sends nothing.
    Some(m.rest)
}

/// Make conversation `n` of the pane's agent the live channel. `false` (with
/// the reason shown) when it cannot be addressed or switched to.
fn switch_to(app: &mut App, n: u64) -> bool {
    if app.channel.as_ref().is_some_and(|c| c.ordinal == n) {
        return true;
    }
    if app.streaming {
        app.push_system("still generating — press Ctrl+C to cancel first");
        return false;
    }
    let ch = match mur_channel::ChannelService::open(&app.home)
        .map_err(TargetError::from)
        .and_then(|svc| conversation_at(&svc, n, &app.agent))
    {
        Ok(ch) => ch,
        Err(e) => {
            app.push_error(e.to_string());
            return false;
        }
    };
    super::turn::stop_shell(app, false);
    if let Err(e) = app.switch_channel(&ch.id) {
        app.push_error(format!("could not switch to #{n}: {e:#}"));
        return false;
    }
    true
}
