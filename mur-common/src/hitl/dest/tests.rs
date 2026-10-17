use super::*;

fn scope(cmd: &str) -> Option<String> {
    classify_bash(cmd).map(|s| s.key())
}

/// The command that opened the issue: a heredoc'd read script fed to a
/// stdin shell on a remote host. Every one of these asked again, every
/// time, no matter what the operator had already answered.
#[test]
fn heredoc_read_script_over_ssh_is_grantable() {
    let cmd = "ssh -o BatchMode=yes karajan@people.example.edu 'sh -s' <<'REMOTE'\ncd /home/web/app || exit 1\necho --TAIL--\ntail -n 120 storage/logs/app.log\nREMOTE\n";
    assert_eq!(
        scope(cmd).as_deref(),
        Some("ssh:karajan@people.example.edu:ro")
    );
}

/// The other three shapes the same session produced, which must all land
/// on the SAME key — otherwise "don't ask again" still asks.
#[test]
fn every_read_shape_to_one_host_shares_one_key() {
    let want = Some("ssh:karajan@people.example.edu:ro");
    for cmd in [
        "ssh -o BatchMode=yes karajan@people.example.edu 'cd /home/web/app && echo --TAIL--; tail -n 120 log'",
        "ssh karajan@people.example.edu \"cd /home/web/app && ls -la\"",
        "ssh -p 2222 karajan@people.example.edu tail -n 50 /var/log/syslog",
        "ssh karajan@people.example.edu sh -s <<'REMOTE'\nps aux | grep php\nREMOTE\n",
    ] {
        assert_eq!(scope(cmd).as_deref(), want, "cmd: {cmd}");
    }
}

/// `awk` with the operator's real filter — the `&&` lives inside single
/// quotes and must not be read as a shell operator.
#[test]
fn quoted_operators_are_not_shell_operators() {
    let cmd = "ssh karajan@people.example.edu \"awk '/2026-09-21/ && /PayslipSend/ {n++} END{print n}' storage/logs/app.log\"";
    assert_eq!(
        scope(cmd).as_deref(),
        Some("ssh:karajan@people.example.edu:ro")
    );
}

/// Writes ask, at either end — the promise that lets the read grant be
/// this wide. Each of these appeared in the same diagnosis session.
#[test]
fn writes_are_never_grantable() {
    for cmd in [
        "ssh karajan@people.example.edu 'cat >/tmp/status.php'",
        "ssh karajan@people.example.edu 'php artisan tinker --execute=\"...\"'",
        "ssh karajan@people.example.edu 'rm -rf /tmp/x'",
        "ssh karajan@people.example.edu sudo systemctl restart php-fpm",
        "ssh karajan@people.example.edu \"awk '{print > \\\"/tmp/out\\\"}' f\"",
        "tail -n 5 log > /tmp/copy",
    ] {
        assert_eq!(scope(cmd), None, "must still ask: {cmd}");
    }
}

/// Substitution hides a command from this parser, so it ends the proof
/// even when every visible head reads.
#[test]
fn substitution_refuses_the_whole_command() {
    for cmd in [
        "ssh karajan@people.example.edu \"tail -n 5 $(cat /tmp/which)\"",
        "ssh karajan@people.example.edu \"cat `cat /tmp/which`\"",
        "cat /tmp/$(whoami)",
    ] {
        assert_eq!(scope(cmd), None, "must still ask: {cmd}");
    }
}

/// One grant, one host. A command spanning two hosts belongs to neither,
/// and a second hop is a destination the operator never saw.
#[test]
fn a_grant_covers_exactly_one_destination() {
    assert_eq!(
        scope("ssh a@one.example 'ls' && ssh b@two.example 'ls'"),
        None,
        "two hosts in one command must not ride either host's grant"
    );
    assert_eq!(
        scope("ssh a@one.example 'ssh b@two.example ls'"),
        None,
        "a second hop is a destination the human never named"
    );
}

/// An unquoted heredoc tag is expanded by the LOCAL shell first, so a `$`
/// in the body is local execution wearing a remote command's clothes.
#[test]
fn unquoted_heredoc_tag_with_expansion_is_refused() {
    let expanded =
        "ssh karajan@people.example.edu sh -s <<REMOTE\ntail -n 5 $(whoami).log\nREMOTE\n";
    assert_eq!(scope(expanded), None);
    // The same body with nothing to expand stays grantable.
    let literal = "ssh karajan@people.example.edu sh -s <<REMOTE\ntail -n 5 app.log\nREMOTE\n";
    assert_eq!(
        scope(literal).as_deref(),
        Some("ssh:karajan@people.example.edu:ro")
    );
}

/// Local reads get a scope too, so the same mechanism answers the local
/// half of a session instead of a second, differently-shaped grant.
#[test]
fn local_reads_share_one_local_key() {
    for cmd in [
        "ls -la src/",
        "cat Cargo.toml",
        "git log --oneline -5",
        "cd /tmp && ls",
    ] {
        assert_eq!(scope(cmd).as_deref(), Some("local:ro"), "cmd: {cmd}");
    }
}

/// Local setup around one hop keeps the REMOTE key: the thing being
/// trusted is the host, and an `echo` before it does not change that.
#[test]
fn local_preamble_keeps_the_remote_key() {
    assert_eq!(
        scope("echo checking; ssh karajan@people.example.edu 'ls -la'").as_deref(),
        Some("ssh:karajan@people.example.edu:ro")
    );
}

/// An interactive login runs whatever the operator types next, which is
/// not a command this gate ever saw.
#[test]
fn a_bare_login_is_not_a_read() {
    assert_eq!(scope("ssh karajan@people.example.edu"), None);
}

/// An unterminated heredoc means the body was never seen. Fail closed.
#[test]
fn unterminated_heredoc_is_refused() {
    assert_eq!(scope("ssh h 'sh -s' <<'EOF'\nls\n"), None);
}

/// Case cannot buy two grants for one host.
#[test]
fn host_case_is_normalised() {
    assert_eq!(
        scope("ssh Karajan@People.Example.EDU ls").as_deref(),
        Some("ssh:karajan@people.example.edu:ro")
    );
}

/// A quoted argument is DATA, not shell syntax. `tokenize` already strips the
/// quotes, so the metacharacters inside one must not push the segment out of
/// the read lane — otherwise the commonest read shape in a review session
/// (`gh pr view … | jq -r '"m=\(.mergeable)"'`) prompts every single time.
#[test]
fn quoted_metachars_stay_in_the_read_lane() {
    for cmd in [
        r#"gh pr view 1719 --json mergeable | jq -r '"m=\(.mergeable)"'"#,
        r#"gh pr view 1719 --json state | jq -r '.state + "!"'"#,
        r#"git log --oneline -5 | awk '{print $1}'"#,
        r#"grep -rn 'fn foo(' src/"#,
    ] {
        assert_eq!(
            scope(cmd).as_deref(),
            Some("local:ro"),
            "should read: {cmd}"
        );
    }
}

/// The guard the fix must not relax: an UNQUOTED operator still means more
/// than one simple command, and a quoted string that the shell would expand
/// is still refused by `tokenize` itself.
#[test]
fn unquoted_metachars_still_refused() {
    for cmd in ["cat a > b", "ls $(whoami)", r#"echo "$(rm -rf /)""#, "ls &"] {
        assert_eq!(scope(cmd), None, "must not be grantable: {cmd}");
    }
}

/// #1878: agents append `2>/dev/null` / `2>&1` to almost every read. Those
/// only discard or join the command's own streams — they cannot write a
/// file — so a read with one is still a local read, not an unparseable line
/// that falls through to the head deny-list (`gh … 2>/dev/null` was
/// `NetworkEgress`, which auto-approve may never cover).
#[test]
fn stream_discard_and_join_redirects_keep_a_read_local() {
    for cmd in [
        "gh issue list --limit 10 2>/dev/null",
        "gh issue view 1878 2>&1 | head -80",
        "git log --oneline -5 2>/dev/null",
        "ls missing >/dev/null 2>&1",
        "cat a 1>/dev/null",
        "grep -rn x src 2> /dev/null | head",
        "cd /repo && git rev-parse HEAD 2>&1 | head -8",
    ] {
        assert_eq!(scope(cmd).as_deref(), Some("local:ro"), "cmd: {cmd}");
    }
}

/// The exemption is exactly those two shapes. Anything that can land bytes in
/// a real file, read from one, or close a stream is still unmodelled.
#[test]
fn every_other_redirect_is_still_refused() {
    for cmd in [
        "cat a > b",
        "cat a >> b",
        "ls 2>err.log",
        "ls 2>/dev/nullx",
        "ls 2>/dev/null/x",
        "ls '2'>/dev/null",
        "ls >&-",
        "ls 2>&1x",
        "cat < secret",
        "ls &>/dev/null",
        "ls 2>'/dev/null'",
    ] {
        assert_eq!(scope(cmd), None, "cmd: {cmd}");
    }
}
