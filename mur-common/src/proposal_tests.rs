use super::*;
use serde_json::json;

fn ok(v: serde_json::Value) -> Proposal {
    vet(&v).unwrap_or_else(|e| panic!("expected accept, got: {e}"))
}

fn err(v: serde_json::Value) -> VetError {
    vet(&v).expect_err("expected reject")
}

#[test]
fn shell_is_insert_only_with_bang_prefix() {
    let p = ok(json!({"label": "check status", "kind": "shell", "command": "git status"}));
    assert_eq!(p.kind, ProposalKind::Shell("git status".into()));
    assert!(!p.is_executable());
    assert_eq!(p.insert_text().as_deref(), Some("!git status"));
}

#[test]
fn shell_prefix_is_not_doubled() {
    let p = ok(json!({"label": "x", "kind": "shell", "command": "! ls -la"}));
    assert_eq!(p.insert_text().as_deref(), Some("!ls -la"));
}

#[test]
fn slash_is_insert_only_with_slash_prefix() {
    let p = ok(json!({"label": "switch model", "kind": "slash", "command": "/model"}));
    assert_eq!(p.kind, ProposalKind::Slash("model".into()));
    assert!(!p.is_executable());
    assert_eq!(p.insert_text().as_deref(), Some("/model"));
}

#[test]
fn restart_is_executable_and_has_no_insert_text() {
    let p = ok(json!({"label": "restart to apply", "kind": "restart"}));
    assert_eq!(p.kind, ProposalKind::Restart);
    assert!(p.is_executable());
    assert_eq!(p.insert_text(), None);
}

#[test]
fn restart_rejects_a_command_so_no_target_is_expressible() {
    assert_eq!(
        err(json!({"label": "x", "kind": "restart", "command": "other-agent"})),
        VetError::RestartTakesNoCommand
    );
}

#[test]
fn unknown_kind_is_rejected() {
    assert!(matches!(
        err(json!({"label": "x", "kind": "delete_agent"})),
        VetError::UnknownKind(k) if k == "delete_agent"
    ));
}

#[test]
fn missing_fields_are_rejected() {
    assert_eq!(err(json!("nope")), VetError::NotAnObject);
    assert_eq!(
        err(json!({"kind": "restart"})),
        VetError::MissingField("label")
    );
    assert_eq!(err(json!({"label": "x"})), VetError::MissingField("kind"));
    assert_eq!(
        err(json!({"label": "x", "kind": "shell"})),
        VetError::MissingField("command")
    );
    assert_eq!(
        err(json!({"label": 3, "kind": "restart"})),
        VetError::MissingField("label")
    );
}

#[test]
fn empty_and_over_long_are_rejected() {
    assert_eq!(
        err(json!({"label": "  ", "kind": "restart"})),
        VetError::EmptyField("label")
    );
    assert_eq!(
        err(json!({"label": "x", "kind": "shell", "command": "!"})),
        VetError::EmptyField("command")
    );
    let long = "a".repeat(LABEL_MAX_CHARS + 1);
    assert!(matches!(
        err(json!({"label": long, "kind": "restart"})),
        VetError::TooLong { field: "label", .. }
    ));
}

#[test]
fn newline_and_control_chars_are_rejected() {
    for bad in ["ls\nrm -rf ~", "ls\r", "ls\u{1b}[2J", "ls\tx"] {
        assert_eq!(
            err(json!({"label": "x", "kind": "shell", "command": bad})),
            VetError::ControlChar("command"),
            "{bad:?}"
        );
    }
    for bad in ["a\nb", "trailing\n", "trailing\r"] {
        assert_eq!(
            err(json!({"label": bad, "kind": "restart"})),
            VetError::ControlChar("label"),
            "{bad:?}"
        );
    }
}

#[test]
fn placeholders_are_rejected() {
    for (cmd, tok) in [
        ("mur agent stop <name>", "<name>"),
        ("mur agent restart {agent}", "{agent}"),
        ("echo {{target}}", "{{target}}"),
    ] {
        assert_eq!(
            err(json!({"label": "x", "kind": "shell", "command": cmd})),
            VetError::Placeholder {
                field: "command",
                token: tok.into()
            },
            "{cmd}"
        );
    }
}

/// The latent bug the spec calls out: today's `RESTART_HINT` would not pass.
#[test]
fn legacy_restart_hint_text_is_rejected() {
    let hint = "restart the agent to apply (mur agent stop <name>, then start it again)";
    assert!(matches!(
        err(json!({"label": hint, "kind": "restart"})),
        VetError::Placeholder { field: "label", .. }
    ));
}

#[test]
fn shell_expansions_are_not_placeholders() {
    ok(json!({"label": "x", "kind": "shell", "command": "echo ${HOME} && ls < in.txt"}));
}

#[test]
fn secret_shaped_strings_are_rejected() {
    let key = format!("sk-{}", "a".repeat(32));
    assert_eq!(
        err(
            json!({"label": "x", "kind": "shell", "command": format!("export OPENAI_API_KEY={key}")})
        ),
        VetError::SecretShaped("command")
    );
    let pat = format!("ghp_{}", "b".repeat(36));
    assert_eq!(
        err(json!({"label": format!("use {pat}"), "kind": "restart"})),
        VetError::SecretShaped("label")
    );
}

#[test]
fn every_error_tells_the_model_what_to_change() {
    let e = err(json!({"label": "x", "kind": "shell", "command": "a <b>"}));
    assert!(e.to_string().contains("concrete value"), "{e}");
    let e = err(json!({"label": "x", "kind": "nope"}));
    assert!(e.to_string().contains("shell, slash, restart"), "{e}");
}

#[test]
fn reply_is_insert_only_verbatim_and_not_vettable() {
    let p = Proposal::reply("yes please");
    assert!(p.is_reply());
    assert!(!p.is_executable());
    assert_eq!(p.insert_text().as_deref(), Some("yes please"));
    // `reply` is murmur-internal: the model cannot propose it.
    let err =
        vet(&serde_json::json!({"label": "x", "kind": "reply", "command": "hi"})).unwrap_err();
    assert!(matches!(err, VetError::UnknownKind(_)));
}
