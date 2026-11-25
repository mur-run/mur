use super::*;

#[test]
fn blank_lines_are_skipped() {
    assert_eq!(classify_plain_line(""), PlainLine::Empty);
    assert_eq!(classify_plain_line("   \n"), PlainLine::Empty);
}

#[test]
fn quit_and_exit_end_plain_mode() {
    assert_eq!(classify_plain_line("/quit\n"), PlainLine::Quit);
    assert_eq!(classify_plain_line("  /exit  "), PlainLine::Quit);
    assert_eq!(classify_plain_line("/QUIT"), PlainLine::Quit);
}

#[test]
fn other_slash_commands_are_held_back() {
    for line in [
        "/channels",
        "/help",
        "/foo",
        "/channels 3",
        "/",
        "/model gpt-x",
    ] {
        assert_eq!(
            classify_plain_line(line),
            PlainLine::SlashCommand,
            "{line:?}"
        );
    }
}

#[test]
fn a_double_slash_sends_the_rest_with_one_slash() {
    assert_eq!(
        classify_plain_line("//etc/hosts is weird"),
        PlainLine::Send("/etc/hosts is weird".into())
    );
    assert_eq!(
        classify_plain_line("//quit"),
        PlainLine::Send("/quit".into())
    );
}

#[test]
fn a_path_is_text_not_a_command() {
    assert_eq!(
        classify_plain_line("/etc/hosts looks odd"),
        PlainLine::Send("/etc/hosts looks odd".into())
    );
}

#[test]
fn ordinary_text_is_sent_trimmed() {
    assert_eq!(
        classify_plain_line("  hello /quit  \n"),
        PlainLine::Send("hello /quit".into())
    );
}

/// Drive the read loop over a scripted stdin, the way a pipe feeds it.
fn run(input: &str) -> (Vec<String>, String) {
    let mut cursor = std::io::Cursor::new(input.as_bytes().to_vec());
    let mut err = Vec::new();
    let mut sent = Vec::new();
    while let Some(m) = next_message(
        |buf| std::io::BufRead::read_line(&mut cursor, buf),
        || {},
        &mut err,
    )
    .unwrap()
    {
        sent.push(m);
    }
    (sent, String::from_utf8(err).unwrap())
}

#[test]
fn the_reported_session_sends_nothing() {
    // #2563: `/channels` then `/quit`, piped, reached the agent as two turns.
    let (sent, err) = run("/channels\n/quit\n");
    assert!(sent.is_empty(), "sent {sent:?}");
    assert_eq!(err.lines().count(), 1);
    assert!(err.contains("slash commands need the TUI"));
}

#[test]
fn quit_stops_reading_the_rest() {
    let (sent, _) = run("hi\n/quit\nnever sent\n");
    assert_eq!(sent, vec!["hi".to_string()]);
}

#[test]
fn eof_ends_without_a_hint() {
    let (sent, err) = run("hi\n\n//x\n");
    assert_eq!(sent, vec!["hi".to_string(), "/x".to_string()]);
    assert!(err.is_empty());
}
