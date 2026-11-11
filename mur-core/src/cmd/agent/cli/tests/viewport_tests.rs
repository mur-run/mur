//! `viewport_tests`, moved out of `cli/mod.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: dedented one level, nothing else.

use super::super::app::App;
use super::super::{
    INLINE_VIEWPORT_HEIGHT, anchor_row, banner_row, prepare_handover, viewport_h_for,
};

/// One height, welcome or not: the viewport never has to shrink, so the
/// first message never purges the screen and re-anchors the transcript on
/// the floor of a tall window (the "mascot drops" report). A chat viewport
/// must still leave the spare rows `insert_before` needs.
#[test]
fn the_viewport_height_never_changes_when_the_first_message_lands() {
    assert_eq!(viewport_h_for(60), INLINE_VIEWPORT_HEIGHT);
    // Short window: one spare row.
    assert_eq!(viewport_h_for(12), 11);
    // Absurdly short: a floor beats a zero-height viewport.
    assert_eq!(viewport_h_for(2), 5);
}

/// The handover purges nothing (`handover::reanchor` is the no-`Purge`
/// variant, so the login transcript survives), and `flush_finished` emits
/// `messages[flushed_upto..]`. Rewinding the cursor here therefore does not
/// "replay" anything — it hands `insert_before` the whole settled
/// transcript a second time, on top of the copy already in scrollback,
/// immediately before the child runs.
/// The viewport must end up bottom-anchored no matter what the cursor
/// query does: the composer lives on the floor, and an unknown cursor row
/// left put would strand pre-murmur shell output on the rows below the
/// composer — rows ratatui never repaints, because it owns only the
/// viewport.
#[test]
fn the_anchor_never_leaves_rows_stranded_below_the_viewport() {
    // Cursor above the anchor row: descend to it (the banner printed
    // above survives untouched).
    assert_eq!(anchor_row(40, 20, Some(3)), Some(20));
    // At or below it: `with_options` scrolls into place on its own.
    assert_eq!(anchor_row(40, 20, Some(20)), None);
    assert_eq!(anchor_row(40, 20, Some(39)), None);
    // Unknown: park on the last row rather than wherever the cursor sits.
    assert_eq!(anchor_row(40, 20, None), Some(39));
    for rows in [5u16, 24, 40, 200] {
        for h in [5u16, 20, rows.saturating_sub(1)] {
            for c in [None, Some(0), Some(rows / 2), Some(rows - 1)] {
                if let Some(row) = anchor_row(rows, h, c) {
                    assert!(row < rows, "rows={rows} h={h} c={c:?} -> {row}");
                    assert!(
                        row >= rows.saturating_sub(h),
                        "anchored above the bottom band: rows={rows} h={h} c={c:?} -> {row}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_handover_does_not_rewind_the_flush_cursor() {
    let mut app = App::test_fixture();
    app.flushed_upto = 3;
    app.flushed_bytes = 17;

    prepare_handover(&mut app, "Anthropic", 60);

    assert_eq!(
        app.flushed_upto, 3,
        "rewinding re-emits the settled transcript into scrollback"
    );
    assert_eq!(app.flushed_bytes, 17, "same, for the partial-message tail");
}

/// Order, not arithmetic: the handover dismisses the welcome, so the
/// height must be read after that. Computed first, this returns the
/// full-window welcome height and `handover::run` would clear — and
/// re-anchor — a viewport almost the size of the screen, scrolling the
/// child's own output off it. A plain notice would NOT do it (that is
/// what keeps the mascot up through `/skills`), hence the explicit flag.
#[test]
fn a_handover_recomputes_the_height_after_its_notice_lands() {
    let mut app = App::test_fixture();
    app.messages.clear();

    let h = prepare_handover(&mut app, "Anthropic", 60);

    assert_eq!(
        h, INLINE_VIEWPORT_HEIGHT,
        "the welcome height must not survive the handover notice"
    );
    assert!(
        app.messages
            .last()
            .is_some_and(|m| m.text.contains("Anthropic")),
        "the notice itself must be in the transcript before the child runs"
    );
}

/// A wipe left pending runs `purge_and_reanchor` on the pass after the
/// child exits — `Clear(ClearType::Purge)` taking the login transcript
/// with it.
#[test]
fn a_handover_cancels_a_pending_screen_wipe() {
    let mut app = App::test_fixture();
    app.wants_screen_wipe = true;

    prepare_handover(&mut app, "Anthropic", 60);

    assert!(!app.wants_screen_wipe);
}

/// #2358: on a cleared window the welcome must end on the viewport's anchor
/// row, so it sits on the composer instead of a screenful above it.
#[test]
fn the_welcome_banner_hugs_the_viewport() {
    // 40 rows, 20-row viewport anchored at 20, 8-line banner: rows 12..20.
    assert_eq!(banner_row(40, 20, 8, Some(0)), Some(12));
    // Cursor already past that row (shell output on screen): print in place,
    // never move up over it.
    assert_eq!(banner_row(40, 20, 8, Some(15)), None);
    assert_eq!(banner_row(40, 20, 8, Some(12)), None);
    // Unknown cursor: same fallback as the viewport anchor.
    assert_eq!(banner_row(40, 20, 8, None), Some(39));
    // Banner taller than the room above the viewport: print at the cursor.
    assert_eq!(banner_row(24, 20, 8, Some(0)), None);
    // Absurd lengths saturate rather than wrap.
    assert_eq!(banner_row(40, 20, usize::MAX, Some(0)), None);
    for rows in [5u16, 24, 40, 200] {
        for h in [5u16, 20, rows.saturating_sub(1)] {
            for b in [0usize, 1, 8, 300] {
                for c in [Some(0), Some(rows / 2), Some(rows - 1)] {
                    if let Some(row) = banner_row(rows, h, b, c) {
                        assert!(row < rows);
                        assert!(Some(row) > c, "moved up: rows={rows} h={h} b={b} c={c:?}");
                        // The banner ends exactly where the viewport anchors.
                        assert_eq!(usize::from(row) + b, usize::from(rows - h));
                    }
                }
            }
        }
    }
}
