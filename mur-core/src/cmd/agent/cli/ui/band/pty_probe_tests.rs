//! [DEBUG-2020] Probe: replay a fenced reply through the REAL crossterm
//! backend. Ignored; run it in a real terminal (58x127 to match the report):
//!   cargo test -p mur-core --lib pty_probe_2020 -- --ignored --nocapture
//! PROBE_VH (viewport, default 20), PROBE_CHUNK (chars per delta, default 7),
//! PROBE_HOLD (seconds to keep the screen, default 60). Pass = every 【P0n】,
//! F01..F07, 【T01】【L01-03】【T02】 is visible on screen or in scrollback.

use super::super::render;
use super::flush_finished;
use crate::cmd::agent::cli::app::{App, RenderMode};
use ratatui::backend::CrosstermBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};

fn reply() -> String {
    let mut s = String::new();
    for n in 1..=4 {
        s.push_str(&format!(
            "【P{n:02}】下面是完整的重設密碼指令，這是第 {n} 段很長的中文說明，故意寫長一點讓它在終端機裡折行，看看會不會被切掉。\n\n"
        ));
    }
    s.push_str("```sh\n");
    s.push_str("EMAIL='F01 括號裡那個 email'\nNEWPW='F02 新密碼-至少12個字'\n\n");
    s.push_str("curl -s -X POST -H 'content-type: application/json' \\\n");
    s.push_str(
        "  -d \"{\\\"email\\\":\\\"$EMAIL\\\"}\" http://127.0.0.1:5150/api/auth/forgot F04\n\n",
    );
    s.push_str("TOKEN=$(psql 'postgres://loco:loco@127.0.0.1:5432/si_te_development' -Atc \\\n");
    s.push_str("  \"select reset_token from users where email='$EMAIL'\") F05\n");
    s.push_str("echo \"token: $TOKEN\" F06\n\n");
    s.push_str("curl -s -X POST -H 'content-type: application/json' \\\n");
    s.push_str(
        "  -d \"{\\\"token\\\":\\\"$TOKEN\\\"}\" http://127.0.0.1:5150/api/auth/reset F07\n```\n\n",
    );
    s.push_str("跑完看這三件事：【T01】\n\n- **`token:` 後面是空的**【L01】\n- **最後一行印出 `null`**【L02】\n- **三段要在 30 分鐘內跑完**【L03】\n\n登入成功後就可以接著跑輪 1、2。【T02】\n");
    s
}

#[test]
#[ignore = "probe: needs a real tty (run in tmux)"]
fn pty_probe_2020() {
    let vh: u16 = std::env::var("PROBE_VH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let chunk: usize = std::env::var("PROBE_CHUNK")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7);
    let (cols, rows) = crossterm::terminal::size().unwrap();
    crossterm::terminal::enable_raw_mode().unwrap();
    crossterm::execute!(std::io::stdout(), crossterm::cursor::MoveTo(0, rows - vh)).unwrap();
    let mut term = Terminal::with_options(
        CrosstermBackend::new(std::io::stdout()),
        TerminalOptions {
            viewport: Viewport::Inline(vh),
        },
    )
    .unwrap();
    let mut app = App::test_fixture();
    app.render_mode = RenderMode::Inline;
    app.width = cols;
    app.begin_user_turn("再給我指令一次 被遮住了");
    let reply = reply();
    let chars: Vec<char> = reply.chars().collect();
    for c in chars.chunks(chunk) {
        app.append_delta(&c.iter().collect::<String>(), false);
        flush_finished(&mut term, &mut app, vh).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
    }
    app.finish_agent_turn(reply, None);
    for _ in 0..3 {
        flush_finished(&mut term, &mut app, vh).unwrap();
        term.draw(|f| render(f, &mut app)).unwrap();
    }
    crossterm::terminal::disable_raw_mode().unwrap();
    let hold: u64 = std::env::var("PROBE_HOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    std::thread::sleep(std::time::Duration::from_secs(hold));
}
