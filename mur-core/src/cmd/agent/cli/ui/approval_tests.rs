//! Guards for the docked approval panel (spec 2026-09-29 §3, §7).

use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

use super::{PanelForm, countdown, countdown_style, panel_form};
use crate::cmd::agent::cli::app::{App, ChatMsg, Role};
use crate::cmd::agent::cli::stream::{HitlRequest, deadline_from};
use crate::cmd::agent::cli::theme::{ANSI, MUR};

const W: u16 = 80;

fn req(tool: &str, input: serde_json::Value) -> HitlRequest {
    HitlRequest {
        hitl_id: "h1".into(),
        step_id: None,
        tool_name: tool.into(),
        tool_input: input,
        deadline: Instant::now() + Duration::from_secs(292),
        declared_risk: None,
    }
}

fn edit_req() -> HitlRequest {
    req(
        "edit_file",
        serde_json::json!({
            "file_path": "~/murui-demo/hello.md",
            "old_string": "hello\nsecond line",
            "new_string": "hello world\nsecond line",
        }),
    )
}

/// An app with a little history and `r` open in the slot.
fn gated(r: HitlRequest) -> App {
    let mut app = App::test_fixture();
    app.width = W;
    for i in 1..=6 {
        app.messages
            .push(ChatMsg::for_test(Role::Agent, &format!("reason line {i}")));
    }
    app.hitl = Some(r);
    app
}

fn frame(app: &mut App, h: u16) -> Buffer {
    let mut term = Terminal::new(TestBackend::new(W, h)).unwrap();
    term.draw(|f| super::super::render(f, app)).unwrap();
    term.backend().buffer().clone()
}

fn row(buf: &Buffer, y: u16) -> String {
    (0..buf.area.width)
        .map(|x| buf[(x, y)].symbol().to_string())
        .collect()
}

fn rows(buf: &Buffer) -> Vec<String> {
    (0..buf.area.height).map(|y| row(buf, y)).collect()
}

fn find_row(buf: &Buffer, needle: &str) -> Option<u16> {
    (0..buf.area.height).find(|y| row(buf, *y).contains(needle))
}

/// Top and bottom border rows, in either border type (`ansi` is plain).
fn panel_rows(buf: &Buffer) -> Option<(u16, u16)> {
    let top = find_row(buf, "╭").or_else(|| find_row(buf, "┌"))?;
    let bottom = find_row(buf, "╰").or_else(|| find_row(buf, "└"))?;
    Some((top, bottom))
}

fn bordered(all: &str) -> bool {
    all.contains('╭') || all.contains('┌')
}

/// The four choices or the status-only hint are on screen.
fn decidable(buf: &Buffer) -> bool {
    let all = rows(buf).join("\n");
    let four = ["1 ", "2 ", "3 ", "4 "].iter().all(|n| all.contains(n)) && all.contains("Yes");
    four || all.contains("1-4 · Ctrl+O")
}

#[test]
fn approval_band_is_docked() {
    let mut app = gated(edit_req());
    let buf = frame(&mut app, 40);
    let (top, bottom) = panel_rows(&buf).expect("panel border");
    // Nothing of the transcript inside the panel.
    for y in top..=bottom {
        assert!(
            !row(&buf, y).contains("reason line"),
            "row {y}: {}",
            row(&buf, y)
        );
    }
    // The composer's top rule sits directly under the panel.
    assert!(
        row(&buf, bottom + 1).contains('─'),
        "composer not under the panel:\n{}",
        rows(&buf).join("\n")
    );
    // The status bar is the last row.
    assert!(
        row(&buf, 39).contains("approve edit_file"),
        "{}",
        row(&buf, 39)
    );
}

#[test]
fn approval_shows_diff_for_edit() {
    let mut app = gated(edit_req());
    let all = rows(&frame(&mut app, 40)).join("\n");
    assert!(all.contains("▌-") && all.contains("▌+"), "{all}");
    assert!(!all.contains("\"old_string\""), "{all}");
    assert!(
        all.contains("edit_file · ~/murui-demo/hello.md"),
        "title: {all}"
    );
}

#[test]
fn approval_countdown() {
    assert_eq!(countdown_style(&MUR, Duration::from_secs(30)), MUR.warn);
    assert_eq!(countdown_style(&MUR, Duration::from_secs(31)), MUR.muted);
    assert_eq!(countdown(Duration::from_secs(90)), "1:30");
    assert_eq!(countdown(Duration::from_secs(292)), "4:52");
}

#[test]
fn deadline_from_runtime_timeout() {
    let t0 = Instant::now();
    let d = deadline_from(&serde_json::json!({"timeout_ms": 90_000}), t0);
    assert_eq!(d - t0, Duration::from_secs(90));
    let mut r = edit_req();
    r.deadline = d;
    assert_eq!(countdown(r.remaining(t0)), "1:30");
    // `expire_stale_hitl` retires at the deadline, not a second before.
    assert!(r.remaining(t0 + Duration::from_secs(89)) > Duration::ZERO);
    assert_eq!(r.remaining(t0 + Duration::from_secs(90)), Duration::ZERO);
}

#[test]
fn deadline_legacy_default() {
    let t0 = Instant::now();
    let d = deadline_from(&serde_json::json!({"hitl_id": "h"}), t0);
    assert_eq!(d - t0, crate::hitl::gate::DEFAULT_TIMEOUT);
    // A batch carries one timeout for every call in it.
    let reqs = HitlRequest::from_params(serde_json::json!({
        "timeout_ms": 5_000,
        "calls": [{"hitl_id": "a", "tool_name": "bash"}, {"hitl_id": "b", "tool_name": "bash"}],
    }));
    assert_eq!(reqs[0].deadline, reqs[1].deadline);
}

/// Source-level: `DEFAULT_TIMEOUT` is named only where `deadline` is set.
#[test]
fn no_default_timeout_outside_receipt() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cmd/agent/cli");
    let mut hits = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if !name.ends_with(".rs") || name.contains("test") {
                continue;
            }
            let src = std::fs::read_to_string(&p).unwrap();
            for (n, l) in src.lines().enumerate() {
                let code = l.split("//").next().unwrap_or("");
                if code.contains("DEFAULT_TIMEOUT") {
                    hits.push(format!("{}:{}", p.display(), n + 1));
                }
            }
        }
    }
    assert_eq!(
        hits.len(),
        1,
        "DEFAULT_TIMEOUT outside `deadline_from`: {hits:?}"
    );
    assert!(hits[0].contains("stream.rs"), "{hits:?}");
}

#[test]
fn approval_height_caps_and_folds() {
    let old: String = (0..400).map(|i| format!("old {i}\n")).collect();
    let mut app = gated(req(
        "edit_file",
        serde_json::json!({"file_path": "a.rs", "old_string": old, "new_string": "x"}),
    ));
    let buf = frame(&mut app, 40);
    let (top, bottom) = panel_rows(&buf).unwrap();
    assert!(bottom - top < 24, "panel is {} rows", bottom - top + 1);
    assert!(rows(&buf).join("\n").contains("lines hidden"));
}

#[test]
fn approval_forms_by_height() {
    // The spec's table (40/12/8/5/4) assumes a 2-row empty composer; this
    // one is 3 rows (`INPUT_H_MIN`: rule, text, pad below), so with the
    // status bar 4 rows are never given up and the two short thresholds sit
    // one row higher. The rules themselves are the spec's (`panel_form`).
    let cases = [
        (40, "full"),
        (12, "no-body"),
        (8, "compact"),
        (6, "compact"),
        (5, "status-only"),
        (4, "status-only"),
    ];
    for (h, want) in cases {
        let mut app = gated(edit_req());
        let buf = frame(&mut app, h);
        let all = rows(&buf).join("\n");
        let got = if all.contains("1-4 · Ctrl+O") {
            "status-only"
        } else if !bordered(&all) {
            "compact"
        } else if all.contains("▌+") {
            "full"
        } else {
            "no-body"
        };
        assert_eq!(got, want, "h={h}:\n{all}");
        assert!(decidable(&buf), "h={h}: no way to decide:\n{all}");
        assert!(
            row(&buf, h - 1).contains("approve edit_file"),
            "h={h}: status bar not last:\n{all}"
        );
    }
}

#[test]
fn panel_form_rules() {
    // 4 options → bordered chrome is 7 rows.
    assert_eq!(panel_form(5, 4, 36, 40), PanelForm::Full { body: 5 });
    assert_eq!(panel_form(5, 4, 8, 12), PanelForm::NoBody);
    assert_eq!(panel_form(5, 4, 7, 11), PanelForm::Compact);
    assert_eq!(panel_form(5, 4, 2, 6), PanelForm::Compact);
    assert_eq!(panel_form(5, 4, 1, 5), PanelForm::StatusOnly);
}

#[test]
fn approval_with_multiline_composer() {
    let mut app = gated(edit_req());
    app.set_input("one\ntwo\nthree\nfour\nfive");
    let buf = frame(&mut app, 12);
    let all = rows(&buf).join("\n");
    assert!(!bordered(&all), "expected compact:\n{all}");
    for w in ["one", "two", "three", "four", "five"] {
        assert!(all.contains(w), "composer lost `{w}`:\n{all}");
    }
    assert!(decidable(&buf), "{all}");
}

#[test]
fn approval_with_fleet_rail() {
    // The rail is subtracted before the panel is sized: same arithmetic as
    // `docked_rows`, checked on `panel_form` directly so no fleet fixture is
    // needed. 12 rows, 4 never given up, a 4-row rail: 4 left → compact.
    let avail_without = 12 - 4;
    let avail_with = 12 - 4 - 4;
    assert_eq!(panel_form(5, 4, avail_without, 12), PanelForm::NoBody);
    assert_eq!(panel_form(5, 4, avail_with, 12), PanelForm::Compact);
}

#[test]
fn approval_hides_chooser_and_chip() {
    use crate::cmd::agent::cli::complete::{Candidate, CompletionState};
    let mut app = gated(edit_req());
    let pick = |s: &str| Candidate {
        display: s.into(),
        insert: s.into(),
        desc: String::new(),
        has_children: false,
    };
    app.completion = Some(CompletionState {
        items: vec![pick("built-it"), pick("stuck-here")],
        selected: 0,
        spaced: true,
        current: None,
    });
    let all = rows(&frame(&mut app, 40)).join("\n");
    assert!(
        !all.contains("built-it"),
        "chooser drawn under a gate:\n{all}"
    );
    app.hitl = None;
    let all = rows(&frame(&mut app, 40)).join("\n");
    assert!(all.contains("built-it"), "chooser not restored:\n{all}");
}

#[test]
fn approval_single_surface() {
    let mut app = gated(edit_req());
    app.push_step_started(
        "s1".into(),
        "edit_file".into(),
        serde_json::json!({"file_path": "a.rs"}),
    );
    app.mark_card_awaiting("s1");
    if let Some(r) = &mut app.hitl {
        r.step_id = Some("s1".into());
    }
    let all = rows(&frame(&mut app, 40)).join("\n");
    assert!(bordered(&all), "panel missing:\n{all}");
    assert!(all.contains("awaiting approval"), "marker missing:\n{all}");
    assert!(
        !all.contains("[y]"),
        "an inline decision row is back:\n{all}"
    );
}

#[test]
fn ansi_render_has_no_rgb_and_reverse_only_status_and_focus() {
    let mut app = gated(edit_req());
    app.theme = &ANSI;
    let buf = frame(&mut app, 40);
    let selected = find_row(&buf, "▸ 1").expect("selected option");
    // The composer's text cursor is drawn reversed by the textarea: it is the
    // focused element's caret, one cell, not a decorative fill.
    let caret_row = find_row(&buf, "Type a message").expect("composer");
    let reversed_on = |y: u16| {
        (0..buf.area.width)
            .filter(|x| buf[(*x, y)].modifier.contains(Modifier::REVERSED))
            .count()
    };
    assert!(
        reversed_on(caret_row) <= 1,
        "composer: {}",
        row(&buf, caret_row)
    );
    for y in (0..buf.area.height).filter(|y| *y != caret_row) {
        for x in 0..buf.area.width {
            let c = &buf[(x, y)];
            for col in [c.fg, c.bg] {
                assert!(
                    !matches!(col, Color::Rgb(..) | Color::Indexed(_)),
                    "({x},{y}) painted {col:?}"
                );
            }
            if c.modifier.contains(Modifier::REVERSED) {
                assert!(
                    y == selected || y == buf.area.height - 1,
                    "reverse video at ({x},{y}): {}",
                    row(&buf, y)
                );
            }
        }
    }
}

#[test]
fn option_three_carries_the_warn_glyph_and_four_follows_the_composer() {
    let mut app = gated(edit_req());
    let all = rows(&frame(&mut app, 40)).join("\n");
    let three = all.lines().find(|l| l.contains("3 Yes")).unwrap();
    assert!(
        three.trim_end().ends_with("▲ │"),
        "▲ not at the right edge: {three}"
    );
    assert!(
        all.contains("No — type a reason below first, or Esc"),
        "{all}"
    );
    app.set_input("use the other file please, it is the right one");
    let all = rows(&frame(&mut app, 40)).join("\n");
    assert!(
        all.contains("No — send \"use the other file please, it \u{2026}\" as the reason"),
        "{all}"
    );
}
