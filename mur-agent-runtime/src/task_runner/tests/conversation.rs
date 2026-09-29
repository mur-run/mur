use super::*;

#[tokio::test]
async fn threads_multi_turn_chat_memory() {
    let runner = TaskRunner::new_stub_echo();
    // Turn 1 — no prior context.
    let _ = runner.run_sync(user_turn("first", "t1", None)).await;
    // Turn 2 — threads context.task_id = t1 (the prior reply's id), exactly
    // as the CLI/Hub clients do.
    let _ = runner.run_sync(user_turn("second", "t2", Some("t1"))).await;

    let store = runner.conversations.lock().unwrap();
    // t1 holds just its own turn; t2 accumulated the prior turn + this one.
    assert_eq!(store.map.get("t1").map(|h| h.len()), Some(3));
    let t2 = store.map.get("t2").expect("turn 2 remembered");
    assert_eq!(t2.len(), 6, "2 turns × (user, agent, ledger) = 6");
    // Turn 1's user message survives into turn 2's memory (the bug was that
    // it didn't — every turn started from an empty history).
    match &t2[0] {
        crate::llm::RichMessage::Text { role, content } => {
            assert_eq!(role, "user");
            assert_eq!(content, "first");
        }
        _ => panic!("expected text"),
    }
}

#[test]
fn seed_history_prepends_prior_conversation() {
    use crate::llm::RichMessage;
    let runner = TaskRunner::new_stub_echo();
    runner.conversations.lock().unwrap().remember(
        "ctx".into(),
        vec![
            RichMessage::Text {
                role: "user".into(),
                content: "u1".into(),
            },
            RichMessage::Text {
                role: "agent".into(),
                content: "a1".into(),
            },
        ],
    );
    let input = mur_common::a2a::Message {
        role: "user".into(),
        parts: vec![MessagePart::Text { text: "u2".into() }],
    };
    // With context → [system, prior user, prior agent, current user].
    let seeded = seed_history("SYS".into(), None, runner.stored_prior(Some("ctx")), &input);
    assert_eq!(seeded.len(), 4);
    assert!(
        matches!(&seeded[0], RichMessage::Text { role, content } if role == "system" && content == "SYS")
    );
    assert!(matches!(&seeded[3], RichMessage::Text { role, .. } if role == "user"));
    // Without context → just system + the current user message (old behavior).
    assert_eq!(
        seed_history("SYS".into(), None, runner.stored_prior(None), &input).len(),
        2
    );
}

fn text(role: &str, content: &str) -> crate::llm::RichMessage {
    crate::llm::RichMessage::Text {
        role: role.into(),
        content: content.into(),
    }
}

fn user_input(t: &str) -> mur_common::a2a::Message {
    mur_common::a2a::Message {
        role: "user".into(),
        parts: vec![MessagePart::Text { text: t.into() }],
    }
}

#[test]
fn seed_history_places_pinned_block_at_index_1_as_user_text() {
    use crate::llm::RichMessage;
    let runner = TaskRunner::new_stub_echo();
    runner
        .conversations
        .lock()
        .unwrap()
        .remember("ctx".into(), vec![text("user", "u1"), text("agent", "a1")]);
    let block = "<project_instructions root=\"/r\">x</project_instructions>";
    let seeded = seed_history(
        "SYS".into(),
        Some(block.into()),
        runner.stored_prior(Some("ctx")),
        &user_input("u2"),
    );
    assert_eq!(seeded.len(), 5);
    assert!(matches!(&seeded[0], RichMessage::Text { role, .. } if role == "system"));
    assert!(
        matches!(&seeded[1], RichMessage::Text { role, content } if role == "user" && content.starts_with("<project_instructions"))
    );
    assert!(
        matches!(&seeded[2], RichMessage::Text { role, content } if role == "user" && content == "u1")
    );
    assert!(
        matches!(&seeded[4], RichMessage::Text { role, content } if role == "user" && content == "u2")
    );
}

#[test]
fn seed_history_without_pinned_matches_baseline() {
    use crate::llm::RichMessage;
    let prior = vec![text("user", "u1"), text("agent", "a1")];
    let seeded = seed_history("SYS".into(), None, prior, &user_input("u2"));
    let got: Vec<(String, String)> = seeded
        .iter()
        .map(|m| match m {
            RichMessage::Text { role, content } => (role.clone(), content.clone()),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    let want = [
        ("system", "SYS"),
        ("user", "u1"),
        ("agent", "a1"),
        ("user", "u2"),
    ]
    .map(|(r, c)| (r.to_string(), c.to_string()));
    assert_eq!(got, want);
}

#[test]
fn pinned_cap_is_half_the_history_budget_capped_at_max() {
    assert_eq!(pinned_cap_bytes(8_000), 16_000);
    assert_eq!(pinned_cap_bytes(100_000), 32 * 1024);
    assert_eq!(pinned_cap_bytes(0), 0);
}

/// Three 12-char turns (3 tokens each), budget 10, a 24-char block (6
/// tokens) → room 4 → only the newest turn fits, and it lands after the
/// block.
#[test]
fn send_time_trim_keeps_pinned_and_newest_turn() {
    use crate::llm::RichMessage;
    let prior = vec![
        text("user", "aaaaaa"),
        text("agent", "aaaaaa"),
        text("user", "bbbbbb"),
        text("agent", "bbbbbb"),
        text("user", "cccccc"),
        text("agent", "cccccc"),
    ];
    let block = "x".repeat(24);
    let trimmed = trim_for_send(prior, 10, block.len());
    assert_eq!(trimmed.len(), 2);
    assert_eq!(estimated_tokens(&trimmed), 3);
    let seeded = seed_history(
        "SYS".into(),
        Some(block.clone()),
        trimmed,
        &user_input("now"),
    );
    assert!(
        matches!(&seeded[1], RichMessage::Text { role, content } if role == "user" && *content == block)
    );
    assert!(
        matches!(&seeded[2], RichMessage::Text { role, content } if role == "user" && content == "cccccc")
    );
    assert!(
        matches!(&seeded[3], RichMessage::Text { role, content } if role == "agent" && content == "cccccc")
    );
    assert_eq!(seeded.len(), 5);
}

#[test]
fn send_time_trim_never_drops_the_last_turn() {
    let only = vec![
        text("user", &"x".repeat(40)),
        text("agent", &"y".repeat(40)),
    ];
    assert_eq!(estimated_tokens(&only), 20);
    assert_eq!(trim_room(10, 24), 4);
    assert_eq!(trim_for_send(only, 10, 24).len(), 2);
}

/// Spec §5.4: `room` and `estimated_tokens` share one divisor. A literal
/// A hard-coded divisor slipping into either side would split them.
#[test]
fn room_and_estimated_tokens_use_the_same_divisor() {
    let n = 400;
    let msg = text("user", &"z".repeat(n));
    let budget = 1_000;
    assert_eq!(
        estimated_tokens(std::slice::from_ref(&msg)),
        budget - trim_room(budget, n)
    );
    assert_eq!(estimated_tokens(&[msg]), 100);
}

/// #1199: a restart used to drop the conversation. The store is rebuilt from
/// scratch here — a fresh `map`, as a new process has — and must still find
/// the turn the caller threads back to it.
#[tokio::test]
async fn a_stub_turn_is_remembered_with_a_narrative_only_ledger() {
    use crate::llm::RichMessage;
    let runner = TaskRunner::new_stub_echo();
    let _ = runner.run_sync(user_turn("first", "t1", None)).await;
    let store = runner.conversations.lock().unwrap();
    let h = store.map.get("t1").expect("remembered");
    assert_eq!(h.len(), 3, "user, agent, ledger: {h:?}");
    match &h[2] {
        RichMessage::TurnLedger { memory, .. } => {
            assert!(memory.narrative_only);
            assert_eq!(memory.attachments, 0);
            assert!(memory.tools.is_empty());
        }
        other => panic!("expected TurnLedger, got {other:?}"),
    }
}

#[test]
fn remember_turn_reads_the_ledger_part_and_counts_images() {
    use crate::llm::RichMessage;
    let runner = TaskRunner::new_stub_echo();
    let mut ledger = crate::turn_ledger::TurnLedger::default();
    ledger.record(crate::turn_ledger::Action {
        tool: "read_file".into(),
        target: "/x/info.txt".into(),
        outcome: crate::turn_ledger::Outcome::Failed("EDEADLK".into()),
        excerpt: None,
    });
    let reply = settle("prose".into(), &ledger);
    let input = Message {
        role: "user".into(),
        parts: vec![
            MessagePart::Text {
                text: "read it".into(),
            },
            MessagePart::Data {
                mime_type: "image/png".into(),
                data: serde_json::json!({ "base64": "QkFTRTY0" }),
            },
        ],
    };
    runner.remember_turn("k1", None, &input, &reply);
    let store = runner.conversations.lock().unwrap();
    let h = store.map.get("k1").expect("remembered");
    assert_eq!(h.len(), 3);
    // The image itself is not stored (as before); the fact of it is.
    assert!(
        matches!(&h[0], RichMessage::Text { role, content } if role == "user" && content == "read it")
    );
    // A failed action warrants a settlement card, which `settle` appends
    // to the text — so the stored reply starts with the prose, not equals it.
    assert!(
        matches!(&h[1], RichMessage::Text { role, content } if role == "agent" && content.starts_with("prose"))
    );
    match &h[2] {
        RichMessage::TurnLedger { memory, .. } => {
            assert_eq!(memory.attachments, 1);
            assert!(!memory.narrative_only);
            assert_eq!(memory.tools[0].error.as_deref(), Some("EDEADLK"));
        }
        other => panic!("expected TurnLedger, got {other:?}"),
    }
}

#[test]
fn a_malformed_ledger_part_falls_back_to_narrative_only() {
    use crate::llm::RichMessage;
    let runner = TaskRunner::new_stub_echo();
    let reply = Message {
        role: "agent".into(),
        parts: vec![
            MessagePart::Text {
                text: "prose".into(),
            },
            MessagePart::Data {
                mime_type: TURN_LEDGER_MIME.into(),
                data: serde_json::json!({ "not": "a ledger" }),
            },
        ],
    };
    let input = Message {
        role: "user".into(),
        parts: vec![MessagePart::Text { text: "hi".into() }],
    };
    runner.remember_turn("k2", None, &input, &reply);
    let store = runner.conversations.lock().unwrap();
    let h = store.map.get("k2").expect("remembered");
    assert!(matches!(&h[2], RichMessage::TurnLedger { memory, .. } if memory.narrative_only));
}

/// 2026-09-18, channel 01a0b304: twenty-four text-only pairs of
/// "short command → report claiming completion" and a reply produced by
/// one model call with zero tool calls. Locks the mechanism, not the
/// model: the next turn's message list must carry, immediately before
/// the new user message, the runtime's record that the previous turn ran
/// nothing. Whether the model then calls a tool is its business.
#[test]
fn the_turn_before_a_new_message_says_whether_it_ran_anything() {
    use crate::llm::RichMessage;
    let runner = TaskRunner::new_stub_echo();
    {
        let mut store = runner.conversations.lock().unwrap();
        let pairs: Vec<RichMessage> = (0..24)
            .flat_map(|i| {
                [
                    RichMessage::Text {
                        role: "user".into(),
                        content: format!("continue {i}"),
                    },
                    RichMessage::Text {
                        role: "agent".into(),
                        content: format!("PR #{} 開好了，全綠。", 1400 + i),
                    },
                ]
            })
            .collect();
        store.remember("prior".into(), pairs);
    }
    // The fabricating turn: prose, no tools.
    let input = Message {
        role: "user".into(),
        parts: vec![MessagePart::Text {
            text: "這疊往 main 推一格".into(),
        }],
    };
    let reply = settle(
        "推了一格：#1402 已 merged。".into(),
        &crate::turn_ledger::TurnLedger::default(),
    );
    runner.remember_turn("fab", Some("prior"), &input, &reply);

    let next = Message {
        role: "user".into(),
        parts: vec![MessagePart::Text {
            text: "真的？".into(),
        }],
    };
    let seeded = seed_history(String::new(), None, runner.stored_prior(Some("fab")), &next);
    let n = seeded.len();
    assert!(
        matches!(&seeded[n - 1], RichMessage::Text { role, content } if role == "user" && content == "真的？")
    );
    match &seeded[n - 2] {
        RichMessage::TurnLedger { memory, .. } => {
            assert!(
                memory.narrative_only,
                "the empty turn must be on the record"
            );
            assert_eq!(memory.attachments, 0);
            let rendered = crate::turn_ledger::render_memory(0, memory);
            assert!(rendered.contains("narrative_only: true"), "{rendered}");
        }
        other => panic!("expected the previous turn's ledger, got {other:?}"),
    }
}

/// 2026-09-18, channel 01a0b304: the reply below came from one model call
/// with zero tool calls. Second line of defence — the user sees the card.
#[test]
fn a_zero_tool_report_of_external_state_carries_the_unverified_card() {
    let reply = settle(
        "推了一格：**#1402 已 merged**，`main` 現在是 `1e0a4d40`。剩下六個全部 rebase 到新 `main`、force-push 完成。".into(),
        &crate::turn_ledger::TurnLedger::default(),
    );
    let text = text_of(&reply);
    assert!(text.contains("─ settlement ─"), "{text}");
    assert!(text.contains("⚠ unverified"), "{text}");
    assert!(!text.contains("nothing ran"), "{text}");
    let ledger = ledger_of(&reply).expect("ledger part");
    assert!(ledger.claims_external_state);
    assert!(ledger.unverified_claim());
}

/// Negative control: a pure chat turn with the same empty ledger earns
/// neither the card nor the flag.
#[test]
fn a_zero_tool_chat_reply_carries_no_card() {
    let reply = settle(
        "哈囉，今天想折騰點什麼？".into(),
        &crate::turn_ledger::TurnLedger::default(),
    );
    let text = text_of(&reply);
    assert!(!text.contains("─ settlement ─"), "{text}");
    let ledger = ledger_of(&reply).expect("ledger part");
    assert!(!ledger.claims_external_state);
    assert!(!ledger.unverified_claim());
}

#[test]
fn conversation_survives_a_restart() {
    use crate::llm::RichMessage;
    let dir = tempfile::tempdir().expect("tempdir");
    let pair = |u: &str, a: &str| {
        vec![
            RichMessage::Text {
                role: "user".into(),
                content: u.into(),
            },
            RichMessage::Text {
                role: "agent".into(),
                content: a.into(),
            },
        ]
    };

    let mut before = ConversationStore {
        dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    before.remember("turn-1".into(), pair("hello", "hi"));

    // A new process: nothing in memory, same directory on disk.
    let after = ConversationStore {
        dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    assert!(after.map.is_empty(), "precondition: memory starts empty");
    let recovered = after.prior(Some("turn-1"));
    assert_eq!(recovered.len(), 2, "history was not recovered from disk");
    assert!(
        matches!(&recovered[0], RichMessage::Text { content, .. } if content == "hello"),
        "recovered the wrong turn: {recovered:?}"
    );

    // Negative control: without a dir the same key recovers nothing, so the
    // assertion above is testing persistence and not some other memory.
    let no_disk = ConversationStore::default();
    assert!(no_disk.prior(Some("turn-1")).is_empty());
}

/// #1200: the cap is a token budget, so many tiny turns are kept where two
/// huge ones are not — and (2026-09-19) trimming removes whole turns, so a
/// ledger never outlives the text it describes.
#[test]
fn history_is_trimmed_by_tokens_in_whole_turns() {
    use crate::llm::RichMessage;
    let msg = |role: &str, n: usize| RichMessage::Text {
        role: role.into(),
        content: "x".repeat(n),
    };
    let ledger = |turn: u32| RichMessage::TurnLedger {
        turn,
        memory: crate::turn_ledger::TurnMemory::empty(0),
    };
    const BUDGET: u64 = 900; // ≈ 3600 chars
    let mut store = ConversationStore {
        budget_tokens: BUDGET,
        ..Default::default()
    };

    // 20 turns × (20 + 20 chars + a ~90-char ledger) ≈ 2600 chars: inside.
    let small: Vec<_> = (0..20)
        .flat_map(|i| [msg("user", 20), msg("agent", 20), ledger(i)])
        .collect();
    assert!(
        estimated_tokens(&small) <= BUDGET,
        "test setup exceeds budget"
    );
    store.remember("small".into(), small);
    assert_eq!(
        store.prior(Some("small")).len(),
        60,
        "trimmed by count, not tokens"
    );

    // An oversized early turn is dropped as a unit — all three messages.
    let big = vec![
        msg("user", 4_000),
        msg("agent", 4_000),
        ledger(1),
        msg("user", 8),
        msg("agent", 8),
        ledger(2),
    ];
    assert!(
        estimated_tokens(&big) > BUDGET,
        "test setup fits the budget"
    );
    store.remember("big".into(), big);
    let kept = store.prior(Some("big"));
    assert_eq!(
        kept.len(),
        3,
        "oversized early turn was not dropped whole: {kept:?}"
    );
    assert!(matches!(&kept[0], RichMessage::Text { role, .. } if role == "user"));
    assert!(matches!(&kept[2], RichMessage::TurnLedger { turn: 2, .. }));

    // Legacy pairs (files written before ledgers) still trim by turn.
    let legacy = vec![
        msg("user", 4_000),
        msg("agent", 4_000),
        msg("user", 8),
        msg("agent", 8),
    ];
    store.remember("legacy".into(), legacy);
    let kept = store.prior(Some("legacy"));
    assert_eq!(kept.len(), 2);
    assert!(matches!(&kept[0], RichMessage::Text { role, .. } if role == "user"));
}

/// The newest turn is stored even when it alone exceeds the budget — the
/// alternative is remembering nothing about the turn that just happened.
#[test]
fn the_newest_turn_is_never_trimmed_away() {
    use crate::llm::RichMessage;
    let mut store = ConversationStore {
        budget_tokens: 10,
        ..Default::default()
    };
    let only = vec![
        RichMessage::Text {
            role: "user".into(),
            content: "x".repeat(500),
        },
        RichMessage::Text {
            role: "agent".into(),
            content: "y".repeat(500),
        },
        RichMessage::TurnLedger {
            turn: 1,
            memory: crate::turn_ledger::TurnMemory::empty(0),
        },
    ];
    store.remember("only".into(), only);
    assert_eq!(store.prior(Some("only")).len(), 3);
}

/// The key arrives over the wire as `context.task_id`, so it must never
/// choose the file path.
#[test]
fn hostile_conversation_key_is_not_written_to_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ConversationStore {
        dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    for bad in ["../escape", "a/b", "", "with space", &"x".repeat(129)] {
        assert!(
            store.path_for(bad).is_none(),
            "key {bad:?} was allowed to name a file"
        );
    }
    assert!(
        store
            .path_for("019eb00c-d646-74a3-8cc8-b16dc1bbacf8")
            .is_some()
    );
}
