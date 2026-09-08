#[cfg(test)]
mod block_boundary_tests {
    use super::super::next_block_end;

    #[test]
    fn commits_one_complete_paragraph_at_a_time() {
        let s = "first para\n\nsecond para\n\nthird";
        let n = next_block_end(s);
        assert_eq!(&s[..n], "first para\n\n");
        // The next call, on the remainder, takes exactly the next block.
        let rest = &s[n..];
        let m = next_block_end(rest);
        assert_eq!(&rest[..m], "second para\n\n");
        // A trailing block with no blank line after it is never committable —
        // it may still grow.
        assert_eq!(next_block_end(&rest[m..]), 0);
    }

    #[test]
    fn a_blank_line_inside_a_fence_is_not_a_boundary() {
        // Committing here would strand an unterminated ``` in scrollback and
        // re-render the fence body once it closes.
        let s = "```rust\nlet a = 1;\n\nlet b = 2;\n```\n\ntail";
        let n = next_block_end(s);
        assert_eq!(&s[..n], "```rust\nlet a = 1;\n\nlet b = 2;\n```\n\n");
        assert_eq!(next_block_end("```\nunclosed\n\nstill inside\n"), 0);
    }

    #[test]
    fn nothing_to_commit_yet() {
        assert_eq!(next_block_end(""), 0);
        assert_eq!(next_block_end("one line, still streaming"), 0);
    }
}

#[cfg(test)]
mod wide_char_tests {
    use super::super::blank_wide_char_continuations;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::{Paragraph, Widget, Wrap};

    #[test]
    fn cjk_continuation_cells_are_blanked() {
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        Paragraph::new("有既有workflow")
            .wrap(Wrap { trim: false })
            .render(area, &mut buf);
        // ratatui fills wide-char continuation cells with a space.
        assert_eq!(buf[(1, 0)].symbol(), " ");
        blank_wide_char_continuations(&mut buf);
        // After the fix: each CJK glyph is followed by an empty (skipped) cell,
        // so the backend prints "有既有" with no interleaved spaces.
        assert_eq!(buf[(0, 0)].symbol(), "有");
        assert_eq!(buf[(1, 0)].symbol(), "");
        assert_eq!(buf[(2, 0)].symbol(), "既");
        assert_eq!(buf[(3, 0)].symbol(), "");
        assert_eq!(buf[(4, 0)].symbol(), "有");
        assert_eq!(buf[(5, 0)].symbol(), "");
        // ASCII run is untouched.
        assert_eq!(buf[(6, 0)].symbol(), "w");
        assert_eq!(buf[(7, 0)].symbol(), "o");
    }
}

#[cfg(test)]
mod scroll_marker_tests {
    use super::super::scroll_marker;

    #[test]
    fn silence_when_everything_fits() {
        assert_eq!(scroll_marker(0, 0), None);
    }

    #[test]
    fn following_the_tail_points_up() {
        assert_eq!(scroll_marker(25, 0).as_deref(), Some(" ↑ 25 more · PgUp "));
    }

    #[test]
    fn scrolled_back_points_the_way_home() {
        assert_eq!(
            scroll_marker(25, 10).as_deref(),
            Some(" ↑ 15 · PgDn to follow ")
        );
    }
}

#[cfg(test)]
mod transcript_chrome_tests {
    use super::super::render_transcript;
    use crate::cmd::agent::cli::app::App;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn painted() -> String {
        let mut app = App::test_fixture();
        let mut term = Terminal::new(TestBackend::new(60, 10)).unwrap();
        term.draw(|f| {
            let area = f.area();
            render_transcript(f, &mut app, area);
        })
        .unwrap();
        term.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<Vec<_>>()
            .chunks(60)
            .map(|row| row.concat())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The agent name is already in the status-bar badge. A full-width rule
    /// spent restating it is the clearest kind of cell that has not earned its
    /// place.
    #[test]
    fn the_transcript_border_carries_no_title() {
        let out = painted();
        assert!(!out.contains("chat ·"), "title is back:\n{out}");
    }

    /// No rule at all. The composer's own titled top border marks the seam
    /// beneath this block, and the top rule only ever hosted the scroll
    /// marker — one line too many on a screen already ruled between speakers.
    #[test]
    fn no_edge_is_drawn() {
        let out = painted();
        let ruled: Vec<usize> = out
            .lines()
            .enumerate()
            .filter(|(_, l)| l.chars().filter(|c| *c == '─').count() > 10)
            .map(|(i, _)| i)
            .collect();
        assert!(
            ruled.is_empty(),
            "expected no rule, got rows {ruled:?}\n{out}"
        );
    }
}

#[cfg(test)]
mod welcome_surface_tests {
    use super::super::{message_block, render_transcript};
    use crate::cmd::agent::cli::app::{App, ChatMsg, Role};
    use crate::cmd::agent::cli::theme::MUR;
    use crate::cmd::agent::cli::welcome::MASCOT_REST;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    fn dump(app: &mut App) -> String {
        let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
        term.draw(|f| render_transcript(f, app, Rect::new(0, 0, 100, 40)))
            .unwrap();
        term.backend().to_string()
    }

    /// A slash command's notice is not a conversation. Before, the first
    /// `/skills` ended the welcome: the mascot vanished, the viewport shrank
    /// to the chat height, and a wiped screen showed five rows of notice over
    /// a blank slab. The welcome stays until someone actually speaks.
    #[test]
    fn a_slash_notice_keeps_the_welcome_on_screen() {
        let mut app = App::test_fixture();
        app.push_system("skin changed to mur");
        let d = dump(&mut app);
        assert!(
            d.contains(MASCOT_REST[1]),
            "mascot gone after a notice:\n{d}"
        );
        assert!(d.contains("skin changed to mur"), "notice missing:\n{d}");

        // Control: the welcome leaves with the head of the band, not with
        // the first spoken turn (see `band_growth_tests`).
        app.messages.push(ChatMsg::for_test(Role::User, "hi"));
        app.flushed_upto = 1;
        let d = dump(&mut app);
        assert!(
            !d.contains(MASCOT_REST[1]),
            "welcome outlived its flush:\n{d}"
        );
    }

    /// Three `/skin` switches drew three rules under the light skin. A rule
    /// marks a change of speaker; a run of notices is one speaker (the UI).
    #[test]
    fn no_turn_draws_a_rule() {
        let mut app = App::test_fixture();
        app.theme = &MUR;
        app.push_system("skin changed to light");
        app.push_system("skin changed to mur");
        app.messages.push(ChatMsg::for_test(Role::User, "hi"));
        app.messages.push(ChatMsg::for_test(Role::Agent, "hello"));
        let text = |i: usize| -> String {
            message_block(&app, i, &app.messages[i], 0, false)
                .iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
                .collect()
        };
        assert!(
            !text(1).contains('─'),
            "notice after notice ruled: {:?}",
            text(1)
        );
        assert!(
            !text(2).contains('─'),
            "user after notice ruled: {:?}",
            text(2)
        );
        assert!(
            !text(3).contains('─'),
            "agent after user ruled: {:?}",
            text(3)
        );
    }
}

/// The live band grows upward: what no longer fits is pushed into scrollback
/// (directly above the band, still on screen), never hidden behind a marker.
#[cfg(test)]
mod band_growth_tests {
    use super::super::super::render;
    use super::super::{flush_finished, render_transcript};
    use crate::cmd::agent::cli::app::{App, ChatMsg, RenderMode, Role};
    use crate::cmd::agent::cli::complete::{Candidate, CompletionState};
    use crate::cmd::agent::cli::welcome::MASCOT_REST;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::{Terminal, TerminalOptions, Viewport};

    fn option(display: &str) -> Candidate {
        Candidate {
            display: display.into(),
            insert: display.into(),
            desc: String::new(),
            has_children: false,
        }
    }

    /// A finished exchange with three suggested replies pending — the shape
    /// from the report: the reply the operator must read to choose.
    fn app_with_open_chooser() -> App {
        let mut app = App::test_fixture();
        app.render_mode = RenderMode::Inline;
        let long = (1..=8)
            .map(|i| format!("earlier line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.messages.push(ChatMsg::for_test(Role::Agent, &long));
        app.messages
            .push(ChatMsg::for_test(Role::User, "先建 Messaging API channel"));
        app.messages.push(ChatMsg::for_test(
            Role::Agent,
            "Messaging API 建好以後跟我說",
        ));
        app.completion = Some(CompletionState {
            items: vec![option("建好了"), option("卡住了"), option("接著建")],
            selected: 0,
            spaced: true,
        });
        app
    }

    /// The chooser takes rows from the band; before, the band answered by
    /// hiding rows above ("↑ 7 more · PgUp") — the reply behind the chooser
    /// read as lost. Now the overflow is flushed up into scrollback, where it
    /// stays readable while the operator picks.
    #[test]
    fn an_open_chooser_pushes_the_reply_up_instead_of_hiding_it() {
        let mut app = app_with_open_chooser();
        let mut term = Terminal::with_options(
            TestBackend::new(100, 60),
            TerminalOptions {
                viewport: Viewport::Inline(20),
            },
        )
        .unwrap();
        flush_finished(&mut term, &mut app, 20).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let d = term.backend().to_string();
        assert!(!d.contains("PgUp"), "rows hidden behind the chooser:\n{d}");
        assert!(
            d.contains("earlier line 1"),
            "flushed reply not on screen:\n{d}"
        );
        assert!(
            d.contains("建好以後跟我說"),
            "latest reply not on screen:\n{d}"
        );
    }

    /// The band's top rule was one more line on a screen full of them. It
    /// only ever carried the scroll marker, which now paints on the first
    /// row by itself when — and only when — rows are hidden.
    #[test]
    fn no_rule_above_the_band_and_the_marker_still_shows_when_needed() {
        let mut app = App::test_fixture();
        app.messages.push(ChatMsg::for_test(Role::User, "hi"));
        app.messages.push(ChatMsg::for_test(Role::Agent, "hello"));
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| render_transcript(f, &mut app, Rect::new(0, 0, 100, 30)))
            .unwrap();
        let d = term.backend().to_string();
        assert!(!d.contains('─'), "a rule above the band:\n{d}");
        assert!(!d.contains("PgUp"), "marker with nothing hidden:\n{d}");

        // Control: squeeze the same transcript so rows really are hidden.
        app.flushed_upto = 0;
        let mut term = Terminal::new(TestBackend::new(100, 3)).unwrap();
        term.draw(|f| render_transcript(f, &mut app, Rect::new(0, 0, 100, 3)))
            .unwrap();
        let d = term.backend().to_string();
        assert!(d.contains("more · PgUp"), "hidden rows unmarked:\n{d}");
    }

    /// The mascot is the head of the band, not a splash that the first turn
    /// replaces: it stays until the band fills and the flush carries it up.
    #[test]
    fn the_mascot_stays_until_the_first_flush() {
        let mut app = App::test_fixture();
        app.messages.push(ChatMsg::for_test(Role::User, "hi"));
        app.messages.push(ChatMsg::for_test(Role::Agent, "hello"));
        let dump = |app: &mut App| {
            let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
            term.draw(|f| render_transcript(f, app, Rect::new(0, 0, 100, 40)))
                .unwrap();
            term.backend().to_string()
        };
        let d = dump(&mut app);
        assert!(d.contains(MASCOT_REST[1]), "mascot gone after a turn:\n{d}");
        assert!(d.contains("hello"), "reply missing under the mascot:\n{d}");

        // Control: once the head is in scrollback the band no longer paints it.
        app.flushed_upto = 1;
        let d = dump(&mut app);
        assert!(!d.contains(MASCOT_REST[1]), "mascot painted twice:\n{d}");
    }
}

/// Spec decisions 4 and 5: no rule between turns, one rule above the
/// composer.
#[cfg(test)]
mod layout_guard_tests {
    use super::super::super::super::app::{App, ChatMsg, Role};
    use super::super::super::super::theme::{ANSI, LIGHT, MUR};
    use super::super::super::render;
    use super::super::render_transcript;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::{Terminal, TerminalOptions, Viewport};

    /// The role label already says the speaker changed; a rule under it was
    /// one more line on a screen full of them. No skin draws one.
    #[test]
    fn no_rule_between_turns_under_any_skin() {
        for (name, theme) in [("ansi", &ANSI), ("light", &LIGHT), ("mur", &MUR)] {
            let mut app = App::test_fixture();
            app.theme = theme;
            app.welcome_dismissed = true;
            app.messages.push(ChatMsg::for_test(Role::User, "hi"));
            app.messages.push(ChatMsg::for_test(Role::Agent, "hello"));
            let mut term = Terminal::new(TestBackend::new(80, 30)).unwrap();
            term.draw(|f| render_transcript(f, &mut app, Rect::new(0, 0, 80, 30)))
                .unwrap();
            let d = term.backend().to_string();
            let ruled = d
                .lines()
                .filter(|l| l.chars().filter(|c| *c == '─').count() > 10)
                .count();
            assert_eq!(ruled, 0, "{name} drew a rule between turns:\n{d}");
        }
    }

    /// One rule above the input row, and the status bar directly under it —
    /// the composer's bottom border marked the same seam the status bar does.
    #[test]
    fn composer_has_one_rule_and_the_status_bar_sits_under_the_input() {
        let mut app = App::test_fixture();
        app.welcome_dismissed = true;
        app.messages.push(ChatMsg::for_test(Role::User, "hi"));
        let mut term = Terminal::with_options(
            TestBackend::new(80, 20),
            TerminalOptions {
                viewport: Viewport::Inline(20),
            },
        )
        .unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
        let d = term.backend().to_string();
        let rows: Vec<&str> = d.lines().map(|l| l.trim_matches('"')).collect();
        let status = rows[rows.len() - 1];
        let pad_below = rows[rows.len() - 2];
        let input = rows[rows.len() - 3];
        let pad_above = rows[rows.len() - 4];
        let rule = rows[rows.len() - 5];
        assert!(status.contains("ready"), "status bar not last:\n{d}");
        assert!(
            pad_below.trim().is_empty() && pad_above.trim().is_empty(),
            "the input text must have a blank row above and below:\n{d}"
        );
        assert!(
            input.contains("Type a message"),
            "input row not two above the status bar:\n{d}"
        );
        assert!(
            rule.contains("message —"),
            "composer rule not above the padded input:\n{d}"
        );
    }
}
