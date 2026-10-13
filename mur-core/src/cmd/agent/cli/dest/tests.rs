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
