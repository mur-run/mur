use super::*;

#[test]
fn a_closed_port_note_covers_only_dials_since_the_call_started() {
    let ev = Events::new("mur");
    let t0 = Instant::now() - Duration::from_secs(1);
    ev.port_closed("live", "old.x", 8080, t0);
    let call = ev.in_flight("live", "t");
    assert!(
        call.closed_port_note().is_none(),
        "earlier dial is not this call's"
    );
    let later = Instant::now() + Duration::from_millis(1);
    ev.port_closed("live", "x.example.com", 3000, later);
    ev.port_closed("live", "x.example.com", 3000, later);
    let note = call.closed_port_note().unwrap();
    assert_eq!(note.lines().count(), 1, "deduped: {note}");
    assert!(note.contains("x.example.com:3000"), "{note}");
    // Another server's call hears nothing.
    assert!(ev.in_flight("fetch", "t").closed_port_note().is_none());
}

#[test]
fn a_stale_closed_port_is_swept_by_any_later_event() {
    let ev = Events::new("mur");
    let t0 = Instant::now();
    ev.port_closed("once", "x.example.com", 3000, t0);
    ev.port_closed("other", "y.example.com", 4000, t0 + ATTRIBUTION_GRACE);
    let g = ev.inner.lock().unwrap();
    assert!(
        !g.closed_ports.contains_key("once"),
        "swept after the grace"
    );
    assert!(g.closed_ports.contains_key("other"));
}

#[test]
fn a_closed_port_does_not_throttle_a_host_denial() {
    let ev = Events::new("mur");
    let t = Instant::now();
    assert!(ev.port_closed("live", "x.example.com", 3000, t).is_some());
    assert!(
        ev.denied("live", "x.example.com", Some(443), false, t)
            .is_some()
    );
    assert!(
        ev.port_closed("live", "x.example.com", 3000, t).is_none(),
        "throttled"
    );
    assert!(ev.port_closed("live", "x.example.com", 8443, t).is_some());
}

#[test]
fn one_event_per_server_host_per_window() {
    let ev = Events::new("mur");
    let (tx, mut rx) = mpsc::channel(8);
    ev.inner.lock().unwrap().sink = Some(tx);
    let t0 = Instant::now();
    let first = ev
        .denied("browser", "img.x.com", Some(443), false, t0)
        .unwrap();
    assert_eq!(first.reason, DenyReason::NotAllowed);
    assert_eq!(first.agent, "mur");
    assert!(
        ev.denied(
            "browser",
            "img.x.com",
            Some(443),
            false,
            t0 + Duration::from_secs(59)
        )
        .is_none()
    );
    // Different host or server: its own window.
    assert!(ev.denied("browser", "cdn.x.com", None, false, t0).is_some());
    assert!(ev.denied("search", "img.x.com", None, true, t0).is_some());
    assert!(
        ev.denied(
            "browser",
            "img.x.com",
            None,
            false,
            t0 + Duration::from_secs(DENIED_EVENT_WINDOW_SECS)
        )
        .is_some()
    );
    let n = rx.try_recv().unwrap();
    assert_eq!(n["method"], DENIED_METHOD);
    assert_eq!(n["params"]["host"], "img.x.com");
}

/// A denial raised while exactly one turn's tool call is using the server
/// is that turn's business: it names the task so the runtime can route it
/// to that turn's connection instead of every connection on the socket.
#[test]
fn a_denial_names_the_one_task_using_the_server() {
    let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
    let _using = h.in_flight("browser", "task-a");
    let d = h
        .events
        .denied("browser", "img.x.com", Some(443), false, Instant::now())
        .unwrap();
    assert_eq!(d.task_id.as_deref(), Some("task-a"));
}

/// Two turns on one shared server cannot be told apart — while both use
/// it, and after both finished, while either page may still be loading —
/// and a child that no turn ever called belongs to no turn: all `None`,
/// which the runtime reads as "broadcast, as before".
#[test]
fn a_denial_is_unattributed_when_zero_or_two_tasks_use_the_server() {
    let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
    let t = Instant::now();
    let a = h.in_flight("browser", "task-a");
    let b = h.in_flight("browser", "task-b");
    let both = h.events.denied("browser", "one.x", None, false, t).unwrap();
    assert_eq!(both.task_id, None);
    b.finish();
    let a_and_trailing_b = h.events.denied("browser", "two.x", None, false, t).unwrap();
    assert_eq!(
        a_and_trailing_b.task_id, None,
        "b's page may still be loading"
    );
    a.finish();
    let nobody = h
        .events
        .denied("browser", "three.x", None, false, t)
        .unwrap();
    assert_eq!(nobody.task_id, None, "a and b both finished recently");
    let never = h.events.denied("fetch", "four.x", None, false, t).unwrap();
    assert_eq!(never.task_id, None, "no turn ever called fetch");
}

/// The leak in the screenshot: `page.goto` returns, the call finishes,
/// and the page keeps loading (a Cloudflare challenge iframe). With no
/// call in flight the denial used to broadcast to every session; it
/// belongs to the turn that just used the server, until the grace ends.
#[test]
fn a_denial_trailing_a_finished_call_goes_to_that_turn() {
    let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
    let t0 = Instant::now();
    h.in_flight("browser", "task-a").finish();
    let d = h
        .events
        .denied("browser", "challenges.x", None, false, t0)
        .unwrap();
    assert_eq!(d.task_id.as_deref(), Some("task-a"));
    let late = t0 + ATTRIBUTION_GRACE + Duration::from_secs(1);
    let d = h
        .events
        .denied("browser", "stale.x", None, false, late)
        .unwrap();
    assert_eq!(d.task_id, None, "past the grace nobody owns it");
    assert!(h.events.inner.lock().unwrap().finished.is_empty());
}

/// One timeout used to switch attribution off for the server until the
/// runtime restarted, so every later denial broadcast. The abandoned
/// turn stays a suspect only for the grace; after that the turn using
/// the server owns its denials again.
#[test]
fn an_abandoned_call_stops_blocking_attribution_after_the_grace() {
    let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
    let t0 = Instant::now();
    drop(h.in_flight("browser", "task-a")); // timed out
    let alone = h
        .events
        .denied("browser", "one.x", None, false, t0)
        .unwrap();
    assert_eq!(
        alone.task_id.as_deref(),
        Some("task-a"),
        "its leftover work is the only suspect"
    );
    let _b = h.in_flight("browser", "task-b");
    let late = t0 + ATTRIBUTION_GRACE + Duration::from_secs(1);
    let d = h
        .events
        .denied("browser", "two.x", None, false, late)
        .unwrap();
    assert_eq!(d.task_id.as_deref(), Some("task-b"));
    assert!(h.events.inner.lock().unwrap().abandoned.is_empty());
}

/// Review on #2106: turn A's page keeps loading after its call returns,
/// and turn B starting a call does not stop it. A denial then could be
/// either turn's, so it must broadcast, never be stamped with B's id
/// (which would route A's own denial away from A). Once A's grace has
/// passed, B is the only suspect again.
#[test]
fn a_finished_turns_trailing_traffic_is_not_stamped_with_another_turn() {
    let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
    let t0 = Instant::now();
    h.in_flight("browser", "task-a").finish();
    let _b = h.in_flight("browser", "task-b");
    let d = h
        .events
        .denied("browser", "challenge.x", None, false, t0)
        .expect("an ambiguous denial still notifies");
    assert_eq!(d.task_id, None, "A or B: ambiguous, so broadcast");
    let later = t0 + ATTRIBUTION_GRACE + Duration::from_secs(1);
    let d = h
        .events
        .denied("browser", "img.x", None, false, later)
        .unwrap();
    assert_eq!(d.task_id.as_deref(), Some("task-b"));
}

/// A call that never reached the server started nothing, so it leaves
/// no suspect behind: the next turn's denials stay its own.
#[test]
fn a_settled_call_leaves_no_trailing_suspect() {
    let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
    h.in_flight("browser", "task-a").settle();
    let _b = h.in_flight("browser", "task-b");
    let d = h
        .events
        .denied("browser", "img.x", None, false, Instant::now())
        .unwrap();
    assert_eq!(d.task_id.as_deref(), Some("task-b"));
}

/// The throttle is per audience: once denials are routed to one turn, a
/// later turn hitting the same host never saw the first notice, so it
/// must get one instead of being swallowed by the other turn's window.
/// While task-a's page may still be loading that notice is a broadcast.
#[test]
fn a_second_turn_gets_its_own_notice_inside_the_window() {
    let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
    let t0 = Instant::now();
    let a = h.in_flight("browser", "task-a");
    assert!(
        h.events
            .denied("browser", "img.x", None, false, t0)
            .is_some()
    );
    assert!(
        h.events
            .denied("browser", "img.x", None, false, t0 + Duration::from_secs(1))
            .is_none(),
        "the same turn stays throttled"
    );
    a.finish();
    let _b = h.in_flight("browser", "task-b");
    let d = h
        .events
        .denied("browser", "img.x", None, false, t0 + Duration::from_secs(2))
        .expect("task-b never saw task-a's notice");
    assert_eq!(
        d.task_id, None,
        "task-a's trailing traffic is still a suspect"
    );
}

/// A call that stopped being waited on (timeout, cancel, panic) may still
/// be running in the server. A later denial could be that leftover work,
/// so it must not be pinned on whichever turn happens to be using the
/// server now: it is broadcast, as before #1907.
#[test]
fn a_call_abandoned_mid_flight_makes_later_denials_unattributed() {
    let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
    let t = Instant::now();
    drop(h.in_flight("browser", "task-a")); // timed out: never finished
    let _b = h.in_flight("browser", "task-b");
    let d = h
        .events
        .denied("browser", "late.x", None, false, t)
        .unwrap();
    assert_eq!(d.task_id, None);
    // Other servers are unaffected.
    let _c = h.in_flight("fetch", "task-b");
    let d = h.events.denied("fetch", "ok.x", None, false, t).unwrap();
    assert_eq!(d.task_id.as_deref(), Some("task-b"));
}

/// Keying the throttle by audience adds an entry per turn that hits a
/// host. In a runtime that lives for weeks those must not pile up: an
/// entry whose window has passed is gone after the next denial, while an
/// entry still inside its window keeps suppressing duplicates.
#[test]
fn expired_throttle_entries_do_not_outlive_their_window() {
    let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
    let window = Duration::from_secs(DENIED_EVENT_WINDOW_SECS);
    let t0 = Instant::now();
    for i in 0..100 {
        // One server each, so every turn is the sole suspect of its own.
        let server = format!("srv-{i}");
        let turn = h.in_flight(&server, &format!("task-{i}"));
        assert!(h.events.denied(&server, "img.x", None, false, t0).is_some());
        turn.finish();
    }
    let live = h.in_flight("browser", "task-live");
    let mid = t0 + window / 2;
    assert!(
        h.events
            .denied("browser", "img.x", None, false, mid)
            .is_some()
    );

    let later = t0 + window;
    assert!(
        h.events
            .denied("browser", "img.x", None, false, later)
            .is_none(),
        "task-live's own window is still open"
    );
    assert_eq!(
        h.events.inner.lock().unwrap().last.len(),
        1,
        "only task-live's entry is still inside its window"
    );
    live.finish();
}

#[test]
fn deny_list_hits_are_tagged_ungrantable() {
    let ev = Events::new("mur");
    let d = ev
        .denied("search", "169.254.169.254", None, true, Instant::now())
        .unwrap();
    assert_eq!(d.reason, DenyReason::DenyListed);
    assert!(!d.grantable());
}

#[test]
fn grant_adds_to_every_token_of_that_server_only() {
    let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
    let a = h.register_server("browser", vec!["shop.example.com".into()], vec![], false);
    let b = h.register_server("browser", vec![], vec![], false);
    let other = h.register_server("fetch", vec![], vec![], false);
    assert_eq!(
        h.grant("browser", "IMG.Shop.Example.com").unwrap(),
        "img.shop.example.com"
    );
    let reg = h.registry.lock().unwrap();
    assert!(reg[&a].allow.contains(&"img.shop.example.com".to_owned()));
    assert!(reg[&b].allow.contains(&"img.shop.example.com".to_owned()));
    assert!(reg[&other].allow.is_empty());
}

#[test]
fn grant_refuses_broad_unknown_and_garbage() {
    let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
    h.register_server("search", vec![], vec!["evil.example".into()], true);
    assert!(matches!(
        h.grant("search", "evil.example"),
        Err(GrantError::DenyListed { .. })
    ));
    assert!(matches!(
        h.grant("nope", "a.com"),
        Err(GrantError::UnknownServer(_))
    ));
    assert!(matches!(
        h.grant("search", "評分，"),
        Err(GrantError::InvalidHost(_))
    ));
}
