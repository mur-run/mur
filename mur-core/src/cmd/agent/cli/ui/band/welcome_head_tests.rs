//! #2358: the welcome sits ON the composer, with the empty rows above it —
//! exercised through the real frame (`ui::render`) in an Inline viewport, not
//! by checking a cursor row in isolation.

use crate::cmd::agent::cli::app::{App, ChatMsg, RenderMode, Role};
use crate::cmd::agent::cli::ui::band::flush_finished;
use crate::cmd::agent::cli::ui::render;
use crate::cmd::agent::cli::welcome::WelcomeHead;
use ratatui::backend::TestBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};

const COLS: u16 = 100;
const ROWS: u16 = 40;
const VIEWPORT: u16 = 20;
/// The welcome's final line (`welcome::welcome_lines`).
const LAST_WELCOME: &str = "for commands · Ctrl+V pastes a screenshot";

fn fresh() -> (App, Terminal<TestBackend>) {
    let mut app = App::test_fixture();
    app.render_mode = RenderMode::Inline;
    app.reset_welcome_head();
    let term = Terminal::with_options(
        TestBackend::new(COLS, ROWS),
        TerminalOptions {
            viewport: Viewport::Inline(VIEWPORT),
        },
    )
    .unwrap();
    (app, term)
}

fn frame(app: &mut App, term: &mut Terminal<TestBackend>) -> Vec<String> {
    flush_finished(term, app, VIEWPORT).unwrap();
    term.draw(|f| render(f, app)).unwrap();
    let buf = term.backend().buffer();
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect()
}

fn row_of(rows: &[String], needle: &str) -> Option<usize> {
    rows.iter().position(|r| r.contains(needle))
}

/// The composer's top border: the first row under the welcome that is not
/// blank. The fix is that it comes immediately after the welcome.
#[test]
fn no_empty_band_separates_the_welcome_from_the_composer() {
    let (mut app, mut term) = fresh();
    assert_eq!(app.welcome_head, WelcomeHead::Live);
    let rows = frame(&mut app, &mut term);
    let dump = rows.join("\n");
    let last = row_of(&rows, LAST_WELCOME).unwrap_or_else(|| panic!("no welcome:\n{dump}"));
    let next = rows[last + 1..]
        .iter()
        .position(|r| !r.trim().is_empty())
        .unwrap_or_else(|| panic!("nothing under the welcome:\n{dump}"));
    assert_eq!(
        next, 0,
        "{next} blank rows between welcome and composer:\n{dump}"
    );
    // And the blank space is above it instead: the mascot is not at the top
    // of the viewport.
    let top = usize::from(ROWS - VIEWPORT);
    assert!(
        rows[top].trim().is_empty(),
        "welcome not bottom-aligned:\n{dump}"
    );
}

/// Messages that fit share the band with the welcome; one that does not
/// pushes the welcome into scrollback first — it is the oldest row.
#[test]
fn the_welcome_leaves_for_scrollback_before_any_message() {
    let (mut app, mut term) = fresh();
    app.messages.push(ChatMsg::for_test(Role::User, "hello"));
    let rows = frame(&mut app, &mut term);
    assert_eq!(app.welcome_head, WelcomeHead::Live, "{}", rows.join("\n"));
    assert!(row_of(&rows, LAST_WELCOME) < row_of(&rows, "hello"));

    let long: String = (1..=7).map(|n| format!("reply line {n}\n\n")).collect();
    app.messages.push(ChatMsg::for_test(Role::Agent, &long));
    let rows = frame(&mut app, &mut term);
    assert_eq!(app.welcome_head, WelcomeHead::Off);
    // Committed above the viewport, not dropped.
    let top = usize::from(ROWS - VIEWPORT);
    let above = row_of(&rows, LAST_WELCOME).expect("welcome lost instead of committed");
    assert!(
        above < top,
        "welcome still in the viewport:\n{}",
        rows.join("\n")
    );
}

/// A handover clears the viewport rows; the welcome must reach scrollback
/// first, the way printed output would have.
#[test]
fn a_handover_commits_the_welcome_first() {
    let (mut app, mut term) = fresh();
    let _ = frame(&mut app, &mut term);
    crate::cmd::agent::cli::prepare_handover(&mut app, "login", ROWS);
    assert_eq!(app.welcome_head, WelcomeHead::Commit);
    let rows = frame(&mut app, &mut term);
    assert_eq!(app.welcome_head, WelcomeHead::Off);
    let top = usize::from(ROWS - VIEWPORT);
    assert!(row_of(&rows, LAST_WELCOME).is_some_and(|r| r < top));
}

/// Without the welcome the band paints from the top as before.
#[test]
fn no_welcome_no_bottom_alignment() {
    let (mut app, mut term) = fresh();
    app.messages.push(ChatMsg::for_test(Role::User, "hi"));
    app.reset_welcome_head();
    assert_eq!(app.welcome_head, WelcomeHead::Off);
    let rows = frame(&mut app, &mut term);
    assert!(row_of(&rows, LAST_WELCOME).is_none());
    let hi = row_of(&rows, "hi").expect("message painted");
    assert!(hi < usize::from(ROWS - VIEWPORT) + 3, "{}", rows.join("\n"));
}
