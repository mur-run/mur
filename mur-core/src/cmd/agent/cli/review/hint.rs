//! P3b-§5.3, AC-P3b-13: the inline `@<unknown>` hint at the send gate.
//! `refresh_hint` runs after every composer edit and resolves only when the
//! text differs from the cached one; the footer reads `current_hint`, which
//! compares strings and touches no filesystem.

use super::state::is_send_gate;
use crate::cmd::agent::cli::app::App;
use crate::cmd::fleet::review::constants::TARGET_UNKNOWN_INLINE_HINT;
use crate::cmd::fleet::review::note::unknown_target;

/// The last text the hint was computed for, and the hint it produced.
#[derive(Debug, Default)]
pub struct InlineHint {
    last: Option<String>,
    hint: Option<String>,
}

impl InlineHint {
    /// Recompute for `text` unless it is the text already cached.
    pub fn refresh(&mut self, text: &str, members: &[String; 2], resolve: impl Fn(&str) -> String) {
        if self.last.as_deref() == Some(text) {
            return;
        }
        self.hint = unknown_target(text, members, resolve)
            .map(|name| TARGET_UNKNOWN_INLINE_HINT.replace("{name}", &name));
        self.last = Some(text.to_string());
    }

    /// The cached hint, only if it was computed for exactly `text`.
    pub fn for_text(&self, text: &str) -> Option<&str> {
        if self.last.as_deref() == Some(text) {
            self.hint.as_deref()
        } else {
            None
        }
    }
}

/// After every composer edit; a no-op unless the send gate is open.
pub fn refresh_hint(app: &mut App) {
    let text = app.input_text();
    let home = app.home.clone();
    let Some(s) = app.review.as_mut() else {
        return;
    };
    if !s.awaiting.as_ref().is_some_and(is_send_gate) {
        return;
    }
    let Some(members) = s.handle.as_ref().map(|h| h.members.clone()) else {
        return;
    };
    s.hint.refresh(&text, &members, |n| {
        crate::a2a_dial::canonicalize_agent_name(&home, n)
    });
}

/// What the footer shows while the send gate is open, if anything.
pub fn current_hint(app: &App) -> Option<String> {
    let s = app.review.as_ref()?;
    if !s.awaiting.as_ref().is_some_and(is_send_gate) {
        return None;
    }
    s.hint.for_text(&app.input_text()).map(str::to_owned)
}
