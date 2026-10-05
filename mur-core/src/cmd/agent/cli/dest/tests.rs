use super::*;

/// What the grant row promises must be what the auto lane honours. These
/// assert the JOIN between `grant_for` (which words the row and stores the
/// key) and `stream_handler` (which looks the key up) — the seam where the
/// original bug lived: the row said "don't ask again for `bash`", the set
/// recorded `bash`, and the lookup never ran because the tier was above
/// the ceiling.
mod grants {
    use super::*;
    use mur_common::hitl::RiskTier;

    fn bash(cmd: &str) -> serde_json::Value {
        serde_json::json!({ "command": cmd })
    }

    /// The exact call from the report. `ssh` is `NetworkEgress`, so the
    /// old code offered a grant that could never fire; now the row is
    /// keyed on the proved destination and the key is real.
    #[test]
    fn a_remote_read_is_granted_on_its_destination_not_on_bash() {
        let cmd = "ssh -o BatchMode=yes karajan@people.example.edu 'tail -n 120 app.log'";
        let g = grant_for("bash", Some(&bash(cmd)), RiskTier::NetworkEgress);
        assert_eq!(
            g.key().as_deref(),
            Some("ssh:karajan@people.example.edu:ro"),
            "a proved remote read must be grantable despite its tier"
        );
        assert!(
            g.label().contains("karajan@people.example.edu"),
            "the row must name the host it is about to trust: {}",
            g.label()
        );
    }

    /// A grant is never offered as a promise it cannot keep. The row says
    /// so in words, and `key()` returns nothing to store.
    #[test]
    fn an_ungrantable_call_says_so_instead_of_pretending() {
        for cmd in [
            "ssh karajan@people.example.edu 'php artisan queue:restart'",
            "ssh karajan@people.example.edu 'rm -rf /tmp/x'",
        ] {
            let g = grant_for("bash", Some(&bash(cmd)), RiskTier::NetworkEgress);
            assert_eq!(g.key(), None, "must not store a grant for: {cmd}");
            assert!(
                g.label().contains("can't skip future asks"),
                "the row must admit it won't stick: {}",
                g.label()
            );
        }
    }

    /// Every read shape in one diagnosis session collapses to ONE key, so
    /// a single answer covers the whole session. This is the actual UX
    /// fix: seven prompts become one.
    #[test]
    fn one_answer_covers_a_whole_diagnosis_session() {
        let session = [
            "ssh -o BatchMode=yes karajan@people.example.edu 'sh -s' <<'REMOTE'\ncd /home/web/app || exit 1\ntail -n 120 storage/logs/app.log\nREMOTE\n",
            "ssh -o BatchMode=yes karajan@people.example.edu 'cd /home/web/app && echo --TAIL--; tail -n 120 log'",
            "ssh karajan@people.example.edu sh -s <<'REMOTE'\ncd /home/web/app || exit 1\nps aux | grep php\nREMOTE\n",
            "ssh karajan@people.example.edu \"awk '/2026-09-21/ && /PayslipSend/ {n++} END{print n}' log\"",
            "ssh -p 2222 karajan@people.example.edu tail -n 50 /var/log/syslog",
        ];
        let granted = grant_for("bash", Some(&bash(session[0])), RiskTier::NetworkEgress)
            .key()
            .expect("the first call must be grantable");
        for cmd in &session[1..] {
            assert_eq!(
                grant_for("bash", Some(&bash(cmd)), RiskTier::NetworkEgress).key(),
                Some(granted.clone()),
                "should have been covered by the first answer: {cmd}"
            );
        }
    }

    /// The refusal names the tier it refused, not a catch-all.
    #[test]
    fn a_refusal_names_its_own_tier() {
        let why = |cmd: &str, t| grant_for("bash", Some(&bash(cmd)), t).label();
        assert!(why("git push", RiskTier::NetworkEgress).contains("sends data off"));
        assert!(why("rm -rf x", RiskTier::Destructive).contains("delete or overwrite"));
        assert!(why("sudo ls", RiskTier::Privileged).contains("elevated"));
    }

    /// A tool with no command still falls back to the old tool-name grant,
    /// so nothing that worked before this module stops working.
    #[test]
    fn non_bash_tools_keep_the_tool_name_grant() {
        assert_eq!(
            grant_for("write_file", None, RiskTier::Write)
                .key()
                .as_deref(),
            Some("write_file")
        );
        assert_eq!(
            grant_for("fleet_run", None, RiskTier::Spend).key(),
            None,
            "a Spend tool was never grantable and still is not"
        );
    }
}

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
        "git status",
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
