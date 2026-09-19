//! #003: copying a soft-wrapped line out of the transcript and pasting it back
//! must not arrive as two lines.
//!
//! Reported: the agent printed a long `mur agent perm allow-read …` path. The
//! pane wrapped it, so the terminal grid holds it as two rows and the copy
//! carries a real `\n`. Pasting that into the composer split the path.

use super::super::*;

/// The reporter's exact payload: one command, cut by the pane's wrap.
const WRAPPED: &str = "mur agent perm allow-read mur\n/private/tmp/claude-501/-Users-david-Projects-mur/3567f7f4-c1a1-4729-916b-83d58388307f/scratchpad/wt-spec";

/// An App that has rendered once, so it knows the width it wrapped at.
fn app_at_width(width: u16) -> App {
    let mut app = App::test_fixture();
    app.wrap_width = width;
    app
}

/// THE BUG: a wrapped path pasted back must stay one line.
#[tokio::test]
async fn pasting_a_wrapped_line_does_not_split_the_input() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = app_at_width(80);

    handle_event(&mut app, Event::Paste(WRAPPED.to_string()), &tx).await;

    assert_eq!(
        app.input.lines().len(),
        1,
        "wrapped paste split the composer: {:?}",
        app.input.lines()
    );
    assert!(
        app.input_text().contains("allow-read mur /private/tmp/"),
        "the two halves were not rejoined: {:?}",
        app.input_text()
    );
}

/// The other half of the contract: newlines the USER typed must survive. A
/// break is only rejoined when the next word could not have fit.
#[tokio::test]
async fn deliberate_newlines_are_preserved() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = app_at_width(80);

    // Two short lines: the second word fits easily, so the user meant it.
    handle_event(&mut app, Event::Paste("hello\nworld".into()), &tx).await;

    assert_eq!(app.input.lines().len(), 2, "a typed newline was eaten");
}

/// Pasted code must arrive byte-for-byte: indentation and blank lines are
/// structure, and are never treated as wrap artifacts however long the line.
#[tokio::test]
async fn indented_and_blank_lines_are_never_rejoined() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = app_at_width(20);

    let code = "fn main() { let x = compute_something_long();\n    let y = 2;\n\nnext";
    handle_event(&mut app, Event::Paste(code.into()), &tx).await;

    assert_eq!(
        app.input_text(),
        code,
        "pasted code was reflowed: {:?}",
        app.input_text()
    );
}

/// Before the first render the wrap width is unknown, and an unknown width may
/// never edit a paste.
#[tokio::test]
async fn unknown_width_leaves_the_paste_untouched() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = app_at_width(0);

    handle_event(&mut app, Event::Paste(WRAPPED.to_string()), &tx).await;

    assert_eq!(
        app.input_text(),
        WRAPPED,
        "paste was altered without a width"
    );
}

/// Markdown pasted from the transcript keeps its list structure — a `-` or `>`
/// opening the next line is authored, not painted.
#[tokio::test]
async fn list_markers_are_never_rejoined() {
    let (tx, _rx) = mpsc::channel(16);
    let mut app = app_at_width(20);

    let md = "here are the things you asked for\n- first\n- second";
    handle_event(&mut app, Event::Paste(md.into()), &tx).await;

    assert_eq!(app.input.lines().len(), 3, "a list was reflowed into prose");
}
