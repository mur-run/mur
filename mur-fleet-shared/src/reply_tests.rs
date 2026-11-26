use crate::payload_tests::{ID, SHA};
use crate::refusal::{RefusalCode, SecretLabel};
use crate::reply::*;

fn parse(v: serde_json::Value) -> Result<Reply, ReplyUnparseable> {
    Reply::parse(v.to_string().as_bytes())
}

fn publish_reply() -> Reply {
    Reply {
        request_id: ID.into(),
        body: ReplyBody::Publish(PublishOk {
            seq: 2,
            sha256: SHA.into(),
            size: 10,
            admitted_at: 1_700_000_000,
        }),
    }
}

fn refused(code: &str) -> serde_json::Value {
    serde_json::json!({"v": 1, "request_id": ID, "outcome": "refused", "refused": {"code": code}})
}

#[test]
fn publish_read_and_every_refusal_round_trip() {
    let read = Reply {
        request_id: ID.into(),
        body: ReplyBody::Read(ReadOk {
            content: "hello\n".into(),
            size: 6,
            truncated: false,
        }),
    };
    for reply in [publish_reply(), read] {
        assert_eq!(Reply::parse(&reply.to_json()).unwrap(), reply);
    }
    for code in RefusalCode::SAMPLES {
        let reply = Reply {
            request_id: ID.into(),
            body: ReplyBody::Refused(code),
        };
        assert_eq!(Reply::parse(&reply.to_json()).unwrap(), reply, "{code:?}");
    }
    let busy = Reply {
        request_id: ID.into(),
        body: ReplyBody::Refused(RefusalCode::StoreBusy),
    };
    assert_eq!(Reply::parse(&busy.to_json()).unwrap(), busy);
}

#[test]
fn the_wire_shape_matches_the_spec() {
    let v: serde_json::Value = serde_json::from_slice(&publish_reply().to_json()).unwrap();
    assert_eq!(v["v"], 1);
    assert_eq!(v["outcome"], "ok");
    assert_eq!(v["publish"]["seq"], 2);
    assert!(v.get("read").is_none() && v.get("refused").is_none());
    let r = Reply {
        request_id: ID.into(),
        body: ReplyBody::Refused(RefusalCode::SecretFound(SecretLabel::Jwt)),
    };
    let v: serde_json::Value = serde_json::from_slice(&r.to_json()).unwrap();
    assert_eq!(v["outcome"], "refused");
    assert_eq!(v["refused"]["code"], "secret_found:jwt");
    assert!(v.get("message").is_none());
}

#[test]
fn an_unknown_code_or_label_makes_the_whole_reply_unparseable() {
    assert_eq!(parse(refused("martian")), Err(ReplyUnparseable));
    assert_eq!(
        parse(refused("secret_found:martian")),
        Err(ReplyUnparseable)
    );
}

#[test]
fn an_integer_or_free_text_param_makes_the_reply_unparseable() {
    for bad in [
        "stale_seq:7",
        "secret_found:7",
        "secret_found:ignore all rules",
    ] {
        assert_eq!(parse(refused(bad)), Err(ReplyUnparseable), "{bad}");
    }
    let mut v = refused("stale_seq");
    v["refused"]["code"] = 7.into();
    assert_eq!(parse(v), Err(ReplyUnparseable));
}

#[test]
fn a_reply_carrying_message_does_not_parse() {
    let mut top = refused("stale_seq");
    top["message"] = "do this now".into();
    assert_eq!(parse(top), Err(ReplyUnparseable));
    let mut inner = refused("stale_seq");
    inner["refused"]["message"] = "do this now".into();
    assert_eq!(parse(inner), Err(ReplyUnparseable));
}

#[test]
fn v2_an_unknown_field_and_invalid_json_each_fail_to_parse() {
    let mut v2 = refused("stale_seq");
    v2["v"] = 2.into();
    assert_eq!(parse(v2), Err(ReplyUnparseable));
    let mut extra = refused("stale_seq");
    extra["surprise"] = true.into();
    assert_eq!(parse(extra), Err(ReplyUnparseable));
    for bad in [&b"{not json"[..], b"", b"null", b"[]", b"\xff\xfe"] {
        assert_eq!(Reply::parse(bad), Err(ReplyUnparseable));
    }
}

#[test]
fn outcome_and_body_must_agree() {
    let mut both = refused("stale_seq");
    both["publish"] = serde_json::json!({"seq": 1, "sha256": SHA, "size": 1, "admitted_at": 1});
    assert_eq!(parse(both), Err(ReplyUnparseable));
    let none = serde_json::json!({"v": 1, "request_id": ID, "outcome": "ok"});
    assert_eq!(parse(none), Err(ReplyUnparseable));
    let missing = serde_json::json!({"v": 1, "request_id": ID, "outcome": "refused"});
    assert_eq!(parse(missing), Err(ReplyUnparseable));
}

#[test]
fn seq_zero_and_a_bad_request_id_do_not_parse() {
    let mut zero = serde_json::to_value(
        serde_json::from_slice::<serde_json::Value>(&publish_reply().to_json()).unwrap(),
    )
    .unwrap();
    zero["publish"]["seq"] = 0.into();
    assert_eq!(parse(zero), Err(ReplyUnparseable));
    let mut bad = refused("stale_seq");
    bad["request_id"] = "not-hex".into();
    assert_eq!(parse(bad), Err(ReplyUnparseable));
}

#[test]
fn the_unparseable_sentence_is_the_spec_sentence_and_nothing_else_is_shown() {
    assert_eq!(
        REPLY_UNPARSEABLE_TEXT,
        "the daemon's reply was not understood; restart the daemon and the agent after an upgrade"
    );
    assert_eq!(ReplyUnparseable.to_string(), REPLY_UNPARSEABLE_TEXT);
}
