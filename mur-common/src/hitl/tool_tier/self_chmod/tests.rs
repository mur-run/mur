use crate::hitl::RiskTier;
use crate::hitl::tool_tier::{classify, classify_with_trigger};

fn tier(cmd: &str) -> RiskTier {
    classify("bash", Some(&serde_json::json!({ "command": cmd })))
}

#[test]
fn self_scoped_chmod_is_write() {
    for cmd in [
        "chmod +x run.sh",
        "chmod u+x scripts/a.sh scripts/b.sh",
        "chmod 755 build/tool",
        "chmod 0644 notes.md",
        "chmod +x $TMPDIR/jail-cleanup.sh",
        "chmod u+rwx,go+rx ${TMPDIR}/x",
        "chmod go-w shared.txt",
        "chmod 755 build/tool && ./build/tool --help",
    ] {
        assert!(
            tier(cmd) < RiskTier::Privileged,
            "cmd: {cmd} → {:?}",
            tier(cmd)
        );
    }
}

#[test]
fn chmod_that_widens_or_leaves_the_workspace_stays_privileged() {
    for cmd in [
        // setuid / setgid / sticky, or write for group/other
        "chmod 4755 tool",
        "chmod 2755 tool",
        "chmod 777 run.sh",
        "chmod 775 run.sh",
        "chmod u+s tool",
        "chmod +t dir",
        "chmod o+w run.sh",
        "chmod +w run.sh",
        "chmod a=rwx run.sh",
        // flags, absolute, home, parent, glob, other variables
        "chmod -R 755 build",
        "chmod 755 /etc/x",
        "chmod 755 ~/.ssh/id_ed25519",
        "chmod 755 ../outside",
        "chmod 755 a/../../b",
        "chmod +x *.sh",
        "chmod +x $W",
        "chmod +x $TMPDIR/../x",
        // the directory or the scratch variable moved first
        "cd /etc && chmod 644 passwd",
        "pushd /etc; chmod 644 hosts",
        "TMPDIR=/etc; chmod 644 $TMPDIR/hosts",
        "export TMPDIR=/etc && chmod 644 ${TMPDIR}/hosts",
        // wrapped, or another privileged head beside it
        "sudo chmod +x run.sh",
        "chmod +x run.sh && sudo ls",
        "chmod +x run.sh; chown me run.sh",
        "bash -c 'chmod +x run.sh'",
        "ssh host chmod +x run.sh",
        "find . -exec chmod 755 {} +",
        // a heredoc may carry the segment somewhere else
        "ssh host sh -s <<'EOF'\nchmod +x run.sh\nEOF",
    ] {
        assert_eq!(tier(cmd), RiskTier::Privileged, "cmd: {cmd}");
    }
}

#[test]
fn a_lowered_chmod_keeps_the_rest_of_the_command_s_tier() {
    assert_eq!(tier("chmod +x a.sh && git push"), RiskTier::NetworkEgress);
    assert_eq!(tier("chmod +x a.sh && rm -rf build"), RiskTier::Destructive);
}

#[test]
fn the_trigger_names_what_set_the_tier() {
    let t = |cmd: &str| classify_with_trigger("bash", Some(&serde_json::json!({ "command": cmd })));
    assert_eq!(t("sudo ls"), (RiskTier::Privileged, Some("sudo".into())));
    assert_eq!(
        t("cd x && python3 -c 'import os; os.chmod(\"x\", 0o777)'"),
        (RiskTier::Privileged, Some("os.chmod".into()))
    );
    assert_eq!(
        t("git push origin main"),
        (RiskTier::NetworkEgress, Some("git push".into()))
    );
    assert_eq!(
        t("curl https://x && rm -f a"),
        (RiskTier::Destructive, Some("rm".into()))
    );
    assert_eq!(
        t("eval $X"),
        (RiskTier::Destructive, Some("opaque_shell".into()))
    );
    assert_eq!(t("cargo build"), (RiskTier::Write, None));
    assert_eq!(t("ls"), (RiskTier::Read, None));
}
