use super::*;
use rfd::MessageDialogResult as R;

#[test]
fn only_the_accept_label_accepts() {
    for r in [
        R::Ok,
        R::Cancel,
        R::Yes,
        R::No,
        R::Custom(CANCEL_LABEL.into()),
        R::Custom(ACCEPT_LABEL.to_lowercase()),
        R::Custom(String::new()),
        R::Custom(QUIT_LABEL.into()),
    ] {
        assert!(!accepted(&r, ACCEPT_LABEL), "{r:?} must not accept");
    }
    assert!(accepted(&R::Custom(ACCEPT_LABEL.into()), ACCEPT_LABEL));
    assert!(accepted(&R::Custom(QUIT_LABEL.into()), QUIT_LABEL));
    assert!(!accepted(&R::Custom(ACCEPT_LABEL.into()), QUIT_LABEL));
}

#[test]
fn cancel_is_the_first_button() {
    match buttons(ACCEPT_LABEL) {
        rfd::MessageButtons::OkCancelCustom(first, second) => {
            assert_eq!(first, CANCEL_LABEL);
            assert_eq!(second, ACCEPT_LABEL);
        }
        other => panic!("unexpected buttons {other:?}"),
    }
}

#[test]
fn the_body_is_the_cli_banner() {
    let flags = OverrideFlags {
        no_hitl: true,
        full_perms: true,
        ..Default::default()
    };
    assert_eq!(body("a", flags), mur_core::cmd::agent::banner("a", flags));
}

#[test]
fn the_confirmation_carries_its_window_into_the_issuer() {
    let flags = OverrideFlags {
        net_unrestricted: true,
        ..Default::default()
    };
    let c = HubConfirmation::for_test("detail-agent-a", flags);
    assert_eq!(c.flags(), flags);
    match c.issuer() {
        Issuer::Hub { window_label } => assert_eq!(window_label, "detail-agent-a"),
        other => panic!("unexpected issuer {other:?}"),
    }
}
