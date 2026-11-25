//! What `--plain` does with one line of input before anything is dialled
//! (#2563). Plain mode has no slash-command parser, so a `/channels` or
//! `/quit` typed out of TUI habit used to go to the agent as a chat message:
//! a model turn, and a stray message in the agent's history.

use std::io::{self, Write};

/// Words that end plain mode, the same as EOF.
const QUIT_WORDS: &[&str] = &["quit", "exit"];

/// Printed to stderr when a slash command is held back, so stdout stays the
/// reply stream a pipe consumes.
pub(super) const SLASH_HINT: &str = "slash commands need the TUI (run without --plain); line not sent. \
     Start the line with // to send text that begins with /.";

/// One input line, classified.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PlainLine {
    /// Blank: skip it.
    Empty,
    /// `/quit` or `/exit`: stop reading.
    Quit,
    /// Any other `/word`: never sent.
    SlashCommand,
    /// Text for the agent, already trimmed (and un-escaped for `//`).
    Send(String),
}

/// Classify one raw line. Pure, so every case is a unit test.
///
/// A slash command is `/` followed by a word of letters, digits, `-` or `_`
/// (or nothing), optionally followed by arguments. A first word holding a
/// second `/` is a path (`/etc/hosts?`), not a command, and is sent as typed.
/// A leading `//` sends the rest with one `/` removed.
pub(super) fn classify_plain_line(line: &str) -> PlainLine {
    let text = line.trim();
    if text.is_empty() {
        return PlainLine::Empty;
    }
    if let Some(rest) = text.strip_prefix("//") {
        return PlainLine::Send(format!("/{rest}"));
    }
    let Some(rest) = text.strip_prefix('/') else {
        return PlainLine::Send(text.to_string());
    };
    let word = rest.split_whitespace().next().unwrap_or("");
    let is_word = word
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !is_word {
        return PlainLine::Send(text.to_string());
    }
    if QUIT_WORDS.iter().any(|q| word.eq_ignore_ascii_case(q)) {
        PlainLine::Quit
    } else {
        PlainLine::SlashCommand
    }
}

/// Read lines until one is for the agent. `Ok(None)` at EOF or `/quit`.
///
/// `read_line` is called once per line so the caller can lock stdin per read
/// (a HITL prompt reads it mid-turn); `prompt` runs before each read.
pub(super) fn next_message(
    mut read_line: impl FnMut(&mut String) -> io::Result<usize>,
    mut prompt: impl FnMut(),
    err: &mut dyn Write,
) -> io::Result<Option<String>> {
    loop {
        prompt();
        let mut line = String::new();
        if read_line(&mut line)? == 0 {
            return Ok(None); // EOF (Ctrl-D / end of pipe)
        }
        match classify_plain_line(&line) {
            PlainLine::Empty => continue,
            PlainLine::Quit => return Ok(None),
            PlainLine::SlashCommand => {
                let _ = writeln!(err, "{SLASH_HINT}");
                let _ = err.flush();
            }
            PlainLine::Send(text) => return Ok(Some(text)),
        }
    }
}

#[cfg(test)]
#[path = "plain_line_tests.rs"]
mod tests;
